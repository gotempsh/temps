use anyhow::{Result};
use async_trait::async_trait;
use bollard::exec::CreateExecOptions;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::{body_full, Docker};
use futures::StreamExt;
use mongodb::bson::doc;
use mongodb::options::ClientOptions;
use mongodb::Client as MongoClient;
use schemars::JsonSchema;
use sea_orm::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info};

use crate::utils::ensure_network_exists;

use super::{ExternalService, RuntimeEnvVar, ServiceConfig, ServiceType};

/// Input configuration for creating a MongoDB service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(title = "MongoDB Configuration", description = "Configuration for MongoDB service")]
pub struct MongodbInputConfig {
    /// MongoDB host address
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// MongoDB port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// MongoDB database name
    #[serde(default = "default_database")]
    #[schemars(example = "example_database", default = "default_database")]
    pub database: String,

    /// MongoDB username
    #[serde(default = "default_username")]
    #[schemars(example = "example_username", default = "default_username")]
    pub username: String,

    /// MongoDB password (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(with = "Option<String>", example = "example_password")]
    pub password: Option<String>,

    /// Docker image to use for MongoDB
    #[serde(default = "default_image")]
    #[schemars(example = "example_image", default = "default_image")]
    pub image: String,

    /// MongoDB version
    #[serde(default = "default_version")]
    #[schemars(example = "example_version", default = "default_version")]
    pub version: String,
}

// Example functions for schemars
fn example_host() -> &'static str {
    "localhost"
}

fn example_port() -> &'static str {
    "27017"
}

fn example_database() -> &'static str {
    "mydatabase"
}

fn example_username() -> &'static str {
    "root"
}

fn example_password() -> &'static str {
    ""
}

fn example_image() -> &'static str {
    "mongo"
}

fn example_version() -> &'static str {
    "8.0"
}

/// Internal runtime configuration for MongoDB service
/// This is what the service uses internally after processing input
/// and what gets saved to the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MongodbRuntimeConfig {
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub image: String,
    pub version: String,
}

impl From<MongodbInputConfig> for MongodbRuntimeConfig {
    fn from(input: MongodbInputConfig) -> Self {
        Self {
            host: input.host,
            port: input.port.unwrap_or_else(|| {
                find_available_port(27017)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "27017".to_string())
            }),
            database: input.database,
            username: input.username,
            password: input.password.unwrap_or_else(generate_password),
            image: input.image,
            version: input.version,
        }
    }
}

fn deserialize_optional_password<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Deserialize as Option to handle missing field
    let opt: Option<String> = Option::deserialize(deserializer)?;

    // Return None if missing or empty (will trigger auto-generation)
    Ok(match opt {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

fn default_host() -> String {
    "localhost".to_string()
}

fn default_database() -> String {
    "admin".to_string()
}

fn default_username() -> String {
    "root".to_string()
}

pub fn generate_password() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

fn default_image() -> String {
    "mongo".to_string()
}

fn default_version() -> String {
    "8.0".to_string()
}


fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn find_available_port(start_port: u16) -> Option<u16> {
    (start_port..start_port + 100).find(|&port| is_port_available(port))
}

pub struct MongodbService {
    name: String,
    config: Arc<RwLock<Option<MongodbRuntimeConfig>>>,
    docker: Arc<Docker>,
}

impl MongodbService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            docker,
        }
    }

    fn get_mongodb_config(&self, service_config: ServiceConfig) -> Result<MongodbRuntimeConfig> {
        // Deserialize input config from parameters
        let input_config: MongodbInputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse MongoDB input configuration: {}", e))?;

        // Transform input config to runtime config (auto-generates password if needed)
        Ok(input_config.into())
    }

    fn get_container_name(&self) -> String {
        format!("temps-mongodb-{}", self.name)
    }

    async fn create_container(&self, docker: &Docker, config: &MongodbRuntimeConfig) -> Result<()> {
        let container_name = self.get_container_name();
        let volume_name = format!("temps-mongodb-{}-data", self.name);

        let create_volume_options = bollard::models::VolumeCreateOptions {
            name: Some(volume_name.clone()),
            driver: Some("local".to_string()),
            ..Default::default()
        };
        docker
            .create_volume(create_volume_options)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create MongoDB volume: {}", e))?;

        info!("Created MongoDB volume: {}", volume_name);

        let env_vars = [format!("MONGO_INITDB_ROOT_USERNAME={}", config.username),
            format!("MONGO_INITDB_ROOT_PASSWORD={}", config.password),
            format!("MONGO_INITDB_DATABASE={}", config.database)];

        let mut container_labels = HashMap::new();
        container_labels.insert("temps.service".to_string(), "mongodb".to_string());
        container_labels.insert("temps.name".to_string(), self.name.clone());

        let image_tag = format!("{}:{}", config.image, config.version);

        // Pull the image first
        info!("Pulling MongoDB image: {}", image_tag);
        let mut stream = docker.create_image(
            Some(bollard::query_parameters::CreateImageOptions {
                from_image: Some(image_tag.clone()),
                ..Default::default()
            }),
            None,
            None,
        );

        while let Some(result) = stream.next().await {
            result.map_err(|e| anyhow::anyhow!("Failed to pull MongoDB image: {}", e))?;
        }

        let host_config = bollard::models::HostConfig {
            port_bindings: Some(HashMap::from([(
                "27017/tcp".to_string(),
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(config.port.clone()),
                }]),
            )])),
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/data/db".to_string()),
                source: Some(volume_name),
                typ: Some(bollard::models::MountTypeEnum::VOLUME),
                ..Default::default()
            }]),
            ..Default::default()
        };

        ensure_network_exists(docker)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to ensure network exists: {:?}", e))?;

        let networking_config = Some(bollard::models::NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                temps_core::NETWORK_NAME.to_string(),
                bollard::models::EndpointSettings {
                    ..Default::default()
                },
            )])),
        });

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(image_tag),
            exposed_ports: Some(HashMap::from([("27017/tcp".to_string(), HashMap::new())])),
            env: Some(env_vars.iter().map(|s| s.to_string()).collect()),
            labels: Some(container_labels),
            host_config: Some(bollard::models::HostConfig {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..host_config
            }),
            networking_config,
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec![
                    "CMD-SHELL".to_string(),
                    format!(
                        "mongosh --eval 'db.adminCommand(\"ping\")' --quiet -u {} -p {} --authenticationDatabase admin",
                        config.username, config.password
                    ),
                ]),
                interval: Some(1000000000), // 1 second
                timeout: Some(3000000000),  // 3 seconds
                retries: Some(3),
                start_period: Some(10000000000),  // 10 seconds
                start_interval: Some(1000000000), // 1 second
            }),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create MongoDB container: {}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start MongoDB container: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container.id)
            .await?;

        info!("MongoDB container {} created and started", container.id);
        Ok(())
    }

    async fn wait_for_container_health(&self, docker: &Docker, container_id: &str) -> Result<()> {
        let mut delay = Duration::from_millis(100);
        let mut total_wait = Duration::from_secs(0);
        let max_wait = Duration::from_secs(60);

        while total_wait < max_wait {
            let info = docker
                .inspect_container(container_id, None::<InspectContainerOptions>)
                .await?;
            if let Some(state) = info.state {
                if state.status == Some(bollard::models::ContainerStateStatusEnum::RUNNING)
                    && state.health.as_ref().and_then(|h| h.status.as_ref())
                        == Some(&bollard::models::HealthStatusEnum::HEALTHY)
                {
                    return Ok(());
                }
            }
            sleep(delay).await;
            total_wait += delay;
            delay = delay.mul_f32(1.5);
        }

        Err(anyhow::anyhow!("MongoDB container health check timed out"))
    }

    async fn get_mongo_client(&self) -> Result<MongoClient> {
        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?
            .clone();

        let connection_string = format!(
            "mongodb://{}:{}@{}:{}/?authSource=admin",
            config.username, config.password, config.host, config.port
        );

        let client_options = ClientOptions::parse(&connection_string).await?;
        let client = MongoClient::with_options(client_options)?;

        Ok(client)
    }

    async fn create_database(&self, db_name: &str) -> Result<()> {
        let client = self.get_mongo_client().await?;
        let db = client.database(db_name);

        // Create a collection to initialize the database
        db.create_collection("_temps_init")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create MongoDB database: {}", e))?;

        info!("Created MongoDB database: {}", db_name);
        Ok(())
    }

    async fn drop_database(&self, db_name: &str) -> Result<()> {
        let client = self.get_mongo_client().await?;
        let db = client.database(db_name);

        db.drop()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to drop MongoDB database: {}", e))?;

        info!("Dropped MongoDB database: {}", db_name);
        Ok(())
    }

    #[allow(dead_code)]
    async fn list_databases(&self) -> Result<Vec<String>> {
        let client = self.get_mongo_client().await?;

        let databases = client
            .list_database_names()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list MongoDB databases: {}", e))?;

        Ok(databases)
    }
}

#[async_trait]
impl ExternalService for MongodbService {
    async fn init(&self, service_config: ServiceConfig) -> Result<HashMap<String, String>> {
        // Parse input config and transform to runtime config
        let mongodb_config = self.get_mongodb_config(service_config.clone())?;
        *self.config.write().await = Some(mongodb_config.clone());

        // Serialize the full runtime config to save to database
        // This ensures auto-generated values (password, port) are persisted
        let runtime_config_json = serde_json::to_value(&mongodb_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize MongoDB runtime config: {}", e))?;

        let runtime_config_map = runtime_config_json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Runtime config is not an object"))?;

        let mut inferred_params = HashMap::new();
        for (key, value) in runtime_config_map {
            if let Some(str_value) = value.as_str() {
                inferred_params.insert(key.clone(), str_value.to_string());
            }
        }

        Ok(inferred_params)
    }

    async fn health_check(&self) -> Result<bool> {
        let client = self.get_mongo_client().await?;

        match client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                error!("MongoDB health check failed: {}", e);
                Ok(false)
            }
        }
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Mongodb
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_connection_info(&self) -> Result<String> {
        let config_guard = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Failed to acquire read lock on config"))?;
        let config = config_guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?;

        Ok(format!(
            "mongodb://{}:{}@{}:{}",
            config.username, config.password, config.host, config.port
        ))
    }

    async fn cleanup(&self) -> Result<()> {
        self.stop().await?;
        self.remove().await?;
        Ok(())
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        // Generate JSON Schema from MongodbInputConfig
        let schema = schemars::schema_for!(MongodbInputConfig);
        serde_json::to_value(schema).ok()
    }

    async fn start(&self) -> Result<()> {
        let docker = &self.docker;
        let container_name = self.get_container_name();
        info!("Starting MongoDB container {}", container_name);

        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if containers.is_empty() {
            let config = self
                .config
                .read()
                .await
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("MongoDB configuration not found"))?
                .clone();
            self.create_container(docker, &config).await?;
        } else {
            docker
                .start_container(
                    &container_name,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start MongoDB container: {}", e))?;
            info!("Started existing MongoDB container: {}", container_name);
        }

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let container_name = self.get_container_name();
        info!("Stopping MongoDB container {}", container_name);

        self.docker
            .stop_container(
                &container_name,
                Some(StopContainerOptions {
                    t: Some(10),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to stop MongoDB container: {}", e))?;

        info!("Stopped MongoDB container: {}", container_name);
        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        let container_name = self.get_container_name();
        info!("Removing MongoDB container {}", container_name);

        // Stop the container first if it's running
        let _ = self.stop().await;

        self.docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    v: true,
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to remove MongoDB container: {}", e))?;

        // Remove the volume
        let volume_name = format!("temps-mongodb-{}-data", self.name);
        let _ = self
            .docker
            .remove_volume(
                &volume_name,
                Some(bollard::query_parameters::RemoveVolumeOptions { force: true }),
            )
            .await;

        info!("Removed MongoDB container and volume");
        Ok(())
    }

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let host = parameters
            .get("host")
            .ok_or_else(|| anyhow::anyhow!("Missing host parameter"))?;
        let port = parameters
            .get("port")
            .ok_or_else(|| anyhow::anyhow!("Missing port parameter"))?;
        let database = parameters
            .get("database")
            .ok_or_else(|| anyhow::anyhow!("Missing database parameter"))?;
        let username = parameters
            .get("username")
            .ok_or_else(|| anyhow::anyhow!("Missing username parameter"))?;
        let password = parameters
            .get("password")
            .ok_or_else(|| anyhow::anyhow!("Missing password parameter"))?;

        let mut env_vars = HashMap::new();
        env_vars.insert("MONGODB_HOST".to_string(), host.clone());
        env_vars.insert("MONGODB_PORT".to_string(), port.clone());
        env_vars.insert("MONGODB_DATABASE".to_string(), database.clone());
        env_vars.insert("MONGODB_USERNAME".to_string(), username.clone());
        env_vars.insert("MONGODB_PASSWORD".to_string(), password.clone());
        env_vars.insert(
            "MONGODB_URL".to_string(),
            format!(
                "mongodb://{}:{}@{}:{}/{}",
                username, password, host, port, database
            ),
        );

        Ok(env_vars)
    }

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let database = parameters
            .get("database")
            .ok_or_else(|| anyhow::anyhow!("Missing database parameter"))?;
        let username = parameters
            .get("username")
            .ok_or_else(|| anyhow::anyhow!("Missing username parameter"))?;
        let password = parameters
            .get("password")
            .ok_or_else(|| anyhow::anyhow!("Missing password parameter"))?;

        let container_name = self.get_container_name();

        let mut env_vars = HashMap::new();
        env_vars.insert("MONGODB_HOST".to_string(), container_name.clone());
        env_vars.insert("MONGODB_PORT".to_string(), "27017".to_string());
        env_vars.insert("MONGODB_DATABASE".to_string(), database.clone());
        env_vars.insert("MONGODB_USERNAME".to_string(), username.clone());
        env_vars.insert("MONGODB_PASSWORD".to_string(), password.clone());
        env_vars.insert(
            "MONGODB_URL".to_string(),
            format!(
                "mongodb://{}:{}@{}:27017/{}",
                username, password, container_name, database
            ),
        );

        Ok(env_vars)
    }

    async fn provision_resource(
        &self,
        _service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<super::LogicalResource> {
        let db_name = format!("{}_{}", project_id, environment);

        // Create the database
        self.create_database(&db_name).await?;

        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?
            .clone();

        let mut credentials = HashMap::new();
        credentials.insert("host".to_string(), config.host);
        credentials.insert("port".to_string(), config.port);
        credentials.insert("database".to_string(), db_name.clone());
        credentials.insert("username".to_string(), config.username);
        credentials.insert("password".to_string(), config.password);

        Ok(super::LogicalResource {
            name: db_name,
            resource_type: "mongodb_database".to_string(),
            credentials,
        })
    }

    async fn deprovision_resource(&self, project_id: &str, environment: &str) -> Result<()> {
        let db_name = format!("{}_{}", project_id, environment);
        self.drop_database(&db_name).await
    }

    fn get_runtime_env_definitions(&self) -> Vec<RuntimeEnvVar> {
        vec![
            RuntimeEnvVar {
                name: "MONGODB_DATABASE".to_string(),
                description: "MongoDB database name for this project/environment".to_string(),
                example: "project1_production".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MONGODB_URL".to_string(),
                description: "Full MongoDB connection URL".to_string(),
                example: "mongodb://username:password@localhost:27017/project1_production"
                    .to_string(),
                sensitive: true, // Contains password
            },
            RuntimeEnvVar {
                name: "MONGODB_HOST".to_string(),
                description: "MongoDB host".to_string(),
                example: "localhost".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MONGODB_PORT".to_string(),
                description: "MongoDB port".to_string(),
                example: "27017".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MONGODB_USERNAME".to_string(),
                description: "MongoDB username".to_string(),
                example: "root".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "MONGODB_PASSWORD".to_string(),
                description: "MongoDB password".to_string(),
                example: "password".to_string(),
                sensitive: true,
            },
        ]
    }

    async fn get_runtime_env_vars(
        &self,
        _config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let db_name = format!("{}_{}", project_id, environment);

        // Create the database if it doesn't exist
        self.create_database(&db_name).await?;

        let config_guard = self.config.read().await;
        let config = config_guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?;

        // Use container name as the Docker hostname for inter-container communication
        let container_name = self.get_container_name();

        let mut env_vars = HashMap::new();
        env_vars.insert("MONGODB_HOST".to_string(), container_name.clone());
        env_vars.insert("MONGODB_PORT".to_string(), "27017".to_string());
        env_vars.insert("MONGODB_DATABASE".to_string(), db_name.clone());
        env_vars.insert("MONGODB_USERNAME".to_string(), config.username.clone());
        env_vars.insert("MONGODB_PASSWORD".to_string(), config.password.clone());
        env_vars.insert(
            "MONGODB_URL".to_string(),
            format!(
                "mongodb://{}:{}@{}:27017/{}",
                config.username, config.password, container_name, db_name
            ),
        );

        Ok(env_vars)
    }

    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let port = service_config
            .parameters
            .get("port")
            .ok_or_else(|| anyhow::anyhow!("Missing port parameter"))?;

        Ok(format!("localhost:{}", port))
    }

    async fn backup_to_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        _backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        _subpath_root: &str,
        _pool: &temps_database::DbConnection,
        _external_service: &temps_entities::external_services::Model,
        _service_config: ServiceConfig,
    ) -> Result<String> {
        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?
            .clone();

        let container_name = self.get_container_name();
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let backup_file = format!("mongodb_backup_{}.gz", timestamp);
        let backup_path = format!("{}/{}", subpath, backup_file);

        info!("Starting MongoDB backup for database: {}", config.database);

        // Create a temporary file for the backup
        let temp_file = tempfile::NamedTempFile::new()?;
        let temp_path = temp_file.path().to_str().unwrap();

        // Execute mongodump inside the container
        let exec_config = CreateExecOptions {
            cmd: Some(vec![
                "mongodump",
                "--archive",
                "--gzip",
                "-u",
                &config.username,
                "-p",
                &config.password,
                "--authenticationDatabase",
                "admin",
                "--db",
                &config.database,
            ]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(&container_name, exec_config)
            .await?;

        let output = self.docker.start_exec(&exec.id, None).await?;

        let mut backup_data = Vec::new();
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(result) = output.next().await {
                match result {
                    Ok(log_output) => match log_output {
                        bollard::container::LogOutput::StdOut { message } => {
                            backup_data.extend_from_slice(&message);
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            let stderr_str = String::from_utf8_lossy(&message);
                            info!("mongodump stderr: {}", stderr_str);
                        }
                        _ => {}
                    },
                    Err(e) => {
                        error!("Error reading exec output: {}", e);
                        return Err(anyhow::anyhow!("Failed to read mongodump output: {}", e));
                    }
                }
            }
        }

        if backup_data.is_empty() {
            return Err(anyhow::anyhow!("Backup data is empty"));
        }

        // Write backup data to temp file
        std::fs::write(temp_path, &backup_data)?;

        info!("MongoDB backup size: {} bytes", backup_data.len());

        // Upload to S3
        let body = aws_sdk_s3::primitives::ByteStream::from_path(temp_path).await?;
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_path)
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload MongoDB backup to S3: {}", e))?;

        info!("MongoDB backup uploaded to S3: {}", backup_path);

        // TODO: Implement backup record creation with new schema
        // The backup entity schema has changed and needs to be updated

        Ok(backup_path)
    }

    async fn restore_from_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        _service_config: ServiceConfig,
    ) -> Result<()> {
        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MongoDB not configured"))?
            .clone();

        let container_name = self.get_container_name();

        info!("Starting MongoDB restore from: {}", backup_location);

        // Download backup from S3
        let response = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(backup_location)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to download MongoDB backup from S3: {}", e))?;

        let backup_data = response
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read backup data: {}", e))?
            .into_bytes();

        info!("Downloaded backup, size: {} bytes", backup_data.len());

        // Create a temporary file for the backup
        let temp_file = tempfile::NamedTempFile::new()?;
        let temp_path = temp_file.path().to_str().unwrap();
        std::fs::write(temp_path, &backup_data)?;

        // Copy backup file to container
        let tar_data = {
            let mut ar = tar::Builder::new(Vec::new());
            ar.append_path_with_name(temp_path, "backup.gz")?;
            ar.finish()?;
            ar.into_inner()?
        };

        self.docker
            .upload_to_container(
                &container_name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/tmp".to_string(),
                    ..Default::default()
                }),
                body_full(tar_data.into()),
            )
            .await?;

        // Execute mongorestore inside the container
        let exec_config = CreateExecOptions {
            cmd: Some(vec![
                "mongorestore",
                "--archive=/tmp/backup.gz",
                "--gzip",
                "-u",
                &config.username,
                "-p",
                &config.password,
                "--authenticationDatabase",
                "admin",
                "--drop",
            ]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let exec = self
            .docker
            .create_exec(&container_name, exec_config)
            .await?;

        let output = self.docker.start_exec(&exec.id, None).await?;

        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(result) = output.next().await {
                match result {
                    Ok(log_output) => match log_output {
                        bollard::container::LogOutput::StdOut { message } => {
                            let stdout_str = String::from_utf8_lossy(&message);
                            info!("mongorestore stdout: {}", stdout_str);
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            let stderr_str = String::from_utf8_lossy(&message);
                            info!("mongorestore stderr: {}", stderr_str);
                        }
                        _ => {}
                    },
                    Err(e) => {
                        error!("Error reading exec output: {}", e);
                        return Err(anyhow::anyhow!("Failed to read mongorestore output: {}", e));
                    }
                }
            }
        }

        // Clean up temporary file in container
        let cleanup_exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    cmd: Some(vec!["rm", "/tmp/backup.gz"]),
                    ..Default::default()
                },
            )
            .await?;

        self.docker.start_exec(&cleanup_exec.id, None).await?;

        info!("MongoDB restore completed successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        assert_eq!(default_host(), "localhost");
        assert_eq!(default_username(), "root");
        assert_eq!(default_image(), "mongo");
        assert_eq!(default_version(), "8.0");
    }

    #[test]
    fn test_generate_password() {
        let password = generate_password();
        assert_eq!(password.len(), 16);
        assert!(password.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_generate_password_uniqueness() {
        // Generate multiple passwords and verify they are unique
        let password1 = generate_password();
        let password2 = generate_password();
        let password3 = generate_password();

        assert_ne!(password1, password2, "Passwords should be unique");
        assert_ne!(password2, password3, "Passwords should be unique");
        assert_ne!(password1, password3, "Passwords should be unique");

        // All should be valid
        assert_eq!(password1.len(), 16);
        assert_eq!(password2.len(), 16);
        assert_eq!(password3.len(), 16);
    }

    #[test]
    fn test_container_name() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = MongodbService::new("test-service".to_string(), docker);
        assert_eq!(service.get_container_name(), "temps-mongodb-test-service");
    }

    #[test]
    fn test_service_type() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = MongodbService::new("test-service".to_string(), docker);
        assert_eq!(service.get_type(), ServiceType::Mongodb);
    }

    #[test]
    fn test_parameter_schema() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = MongodbService::new("test-schema".to_string(), docker);

        // Get the parameter schema
        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();

        // Verify schema structure
        let schema_obj = schema.as_object().expect("Schema should be an object");

        // Check for schema metadata
        assert!(schema_obj.contains_key("$schema"), "Should have $schema field");
        assert!(schema_obj.contains_key("title"), "Should have title field");
        assert!(schema_obj.contains_key("description"), "Should have description field");
        assert!(schema_obj.contains_key("properties"), "Should have properties field");

        // Verify title and description
        assert_eq!(
            schema_obj.get("title").and_then(|v| v.as_str()),
            Some("MongoDB Configuration"),
            "Title should match"
        );

        // Verify properties
        let properties = schema_obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Check for expected fields
        let expected_fields = vec!["host", "port", "database", "username", "password", "image", "version"];
        for field in &expected_fields {
            assert!(
                properties.contains_key(*field),
                "Schema should contain '{}' field",
                field
            );
        }

        // Verify host field has default
        let host_field = properties
            .get("host")
            .and_then(|v| v.as_object())
            .expect("host field should be an object");
        assert_eq!(host_field.get("default").and_then(|v| v.as_str()), Some("localhost"));

        // Verify password field description
        let password_field = properties
            .get("password")
            .and_then(|v| v.as_object())
            .expect("password field should be an object");
        let password_desc = password_field.get("description").and_then(|v| v.as_str());
        assert!(password_desc.is_some());
        assert!(password_desc.unwrap().contains("auto-generated"));
    }
}
