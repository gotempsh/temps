use crate::utils::ensure_network_exists;

use super::{ExternalService, ServiceConfig, ServiceType};
use anyhow::{Context, Result};
use async_trait::async_trait;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::{body_full, Docker};
use futures::{StreamExt, TryStreamExt};
use redis::{aio::ConnectionManager, Client};
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

const REDIS_IMAGE: &str = "redis:7.4.1-alpine";

/// Input configuration for creating a Redis service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(title = "Redis Configuration", description = "Configuration for Redis service")]
pub struct RedisInputConfig {
    /// Redis host address
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// Redis port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// Redis password (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(with = "Option<String>", example = "example_password")]
    pub password: Option<String>,
}

/// Internal runtime configuration for Redis service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub host: String,
    pub port: String,
    pub password: String,
}

impl From<RedisInputConfig> for RedisConfig {
    fn from(input: RedisInputConfig) -> Self {
        Self {
            host: input.host,
            port: input.port.unwrap_or_else(|| {
                find_available_port(6379)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "6379".to_string())
            }),
            password: input.password.unwrap_or_else(generate_password),
        }
    }
}

fn deserialize_optional_password<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

fn default_host() -> String {
    "localhost".to_string()
}

fn generate_password() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

// Schema example functions
fn example_host() -> &'static str {
    "localhost"
}

fn example_port() -> &'static str {
    "6379"
}

fn example_password() -> &'static str {
    "your-secure-password"
}

fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn find_available_port(start_port: u16) -> Option<u16> {
    (start_port..start_port + 100).find(|&port| is_port_available(port))
}

pub struct RedisService {
    name: String,
    config: Arc<RwLock<Option<RedisConfig>>>,
    client: Arc<RwLock<Option<Client>>>,
    connection_manager: Arc<RwLock<Option<ConnectionManager>>>,
    docker: Arc<Docker>,
}

impl RedisService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            client: Arc::new(RwLock::new(None)),
            connection_manager: Arc::new(RwLock::new(None)),
            docker,
        }
    }

    pub async fn get_connection(&self) -> Result<ConnectionManager> {
        let conn = self.connection_manager.read().await;
        match conn.as_ref() {
            Some(c) => Ok(c.clone()),
            None => Err(anyhow::anyhow!("Redis connection not initialized")),
        }
    }

    fn get_container_name(&self) -> String {
        format!("redis-{}", self.name)
    }

    async fn create_container(
        &self,
        docker: &Docker,
        config: &RedisConfig,
        password: &str,
    ) -> Result<()> {
        let container_name = self.get_container_name();

        // Pull the Redis image first
        info!("Pulling Redis image {}", REDIS_IMAGE);

        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = REDIS_IMAGE.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (REDIS_IMAGE.to_string(), "latest".to_string())
        };

        docker
            .create_image(
                Some(bollard::query_parameters::CreateImageOptions {
                    from_image: Some(image_name),
                    tag: Some(tag),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to pull Redis image: {}", e))?;

        // Check if container already exists
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

        if !containers.is_empty() {
            info!("Container {} already exists", container_name);
            return Ok(());
        }

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);

        let container_labels = HashMap::from([
            (service_label_key.as_str(), "redis"),
            (name_label_key.as_str(), self.name.as_str()),
        ]);

        let env_vars = [format!("REDIS_PASSWORD={}", password)];

        let volume_name = format!("redis_data_{}", self.name);
        let host_config = bollard::models::HostConfig {
            port_bindings: Some(HashMap::from([(
                "6379/tcp".to_string(),
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(config.port.to_string()),
                }]),
            )])),
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/data".to_string()),
                source: Some(volume_name.clone()),
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
            image: Some(REDIS_IMAGE.to_string()),
            exposed_ports: Some(HashMap::from([("6379/tcp".to_string(), HashMap::new())])),
            env: Some(env_vars.iter().map(|s| s.as_str().to_string()).collect()),
            labels: Some(
                container_labels
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            cmd: Some(vec![
                "redis-server".to_string(),
                "--appendonly".to_string(),
                "yes".to_string(),
            ]),
            host_config: Some(bollard::models::HostConfig {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..host_config
            }),
            networking_config,
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec!["CMD-SHELL".to_string(), "redis-cli ping".to_string()]),
                interval: Some(1000000000), // 1 second
                timeout: Some(3000000000),  // 3 seconds
                retries: Some(3),
                start_period: Some(5000000000),   // 5 seconds
                start_interval: Some(1000000000), // 1 second
            }),
            ..Default::default()
        };

        // Create volume if it doesn't exist
        match docker
            .create_volume(bollard::models::VolumeCreateOptions {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await
        {
            Ok(_) => info!("Created or reused volume {}", volume_name),
            Err(e) => return Err(anyhow::anyhow!("Failed to create volume: {:?}", e)),
        }

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
            .map_err(|e| anyhow::anyhow!("Failed to create Redis container: {:?}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start Redis container: {:?}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container.id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to wait for Redis container health: {:?}", e))?;

        info!("Redis container {} created and started", container.id);
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

        Err(anyhow::anyhow!("Redis container health check timed out"))
    }

    async fn create_database(&self, name: &str) -> Result<u8> {
        let conn = self.get_connection().await?;

        // Get next available database number
        let db_number = self.get_next_database_number().await?;

        // Store the mapping of name to db number
        redis::cmd("SET")
            .arg(format!("_db_mapping:{}", name))
            .arg(db_number.to_string())
            .query_async::<()>(&mut conn.clone())
            .await?;

        Ok(db_number)
    }

    async fn drop_database(&self, name: &str) -> Result<()> {
        let conn = self.get_connection().await?;

        // Get the database number
        let db_number: Option<String> = redis::cmd("GET")
            .arg(format!("_db_mapping:{}", name))
            .query_async(&mut conn.clone())
            .await?;

        if let Some(db_num) = db_number {
            // Clear all keys in this database
            redis::cmd("SELECT")
                .arg(&db_num)
                .query_async::<()>(&mut conn.clone())
                .await?;

            redis::cmd("FLUSHDB")
                .query_async::<()>(&mut conn.clone())
                .await?;

            // Remove the mapping
            redis::cmd("DEL")
                .arg(format!("_db_mapping:{}", name))
                .query_async::<()>(&mut conn.clone())
                .await?;
        }

        Ok(())
    }

    async fn get_next_database_number(&self) -> Result<u8> {
        // You might want to implement a more sophisticated way to track database numbers
        // For now, we'll just use a simple counter in Redis itself
        let conn = self.get_connection().await?;
        let counter: u8 = redis::cmd("INCR")
            .arg("_db_counter")
            .query_async(&mut conn.clone())
            .await?;

        if counter > 15 {
            return Err(anyhow::anyhow!("No more Redis databases available"));
        }

        Ok(counter)
    }

    fn get_redis_config(&self, service_config: ServiceConfig) -> Result<RedisConfig> {
        // Parse input config and transform to runtime config
        let input_config: RedisInputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse Redis configuration: {}", e))?;

        Ok(RedisConfig::from(input_config))
    }
}

#[async_trait]
impl ExternalService for RedisService {
    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!("Initializing Redis service {:?}", config);

        // Parse input config and transform to runtime config
        let redis_config = self.get_redis_config(config)?;

        // Store runtime config
        *self.config.write().await = Some(redis_config.clone());

        // Create Docker container
        self.create_container(&self.docker, &redis_config, &redis_config.password)
            .await?;

        // Serialize the full runtime config to save to database
        // This ensures auto-generated values (password, port) are persisted
        let runtime_config_json = serde_json::to_value(&redis_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize Redis runtime config: {}", e))?;

        let runtime_config_map = runtime_config_json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Runtime config is not an object"))
            .map_err(|e| anyhow::anyhow!("Runtime config is not an object: {}", e))?;

        let mut inferred_params = HashMap::new();
        for (key, value) in runtime_config_map {
            if let Some(str_value) = value.as_str() {
                inferred_params.insert(key.clone(), str_value.to_string());
            }
        }

        Ok(inferred_params)
    }

    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_redis_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

    async fn health_check(&self) -> Result<bool> {
        let conn = self.get_connection().await?;
        let result: Result<String, redis::RedisError> =
            redis::cmd("PING").query_async(&mut conn.clone()).await;
        Ok(result.is_ok())
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Redis
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_connection_info(&self) -> Result<String> {
        let config = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Failed to read config"))?;

        match &*config {
            Some(cfg) => Ok(format!("redis://localhost:{}", cfg.port)),
            None => Err(anyhow::anyhow!("Redis not configured")),
        }
    }

    async fn cleanup(&self) -> Result<()> {
        *self.connection_manager.write().await = None;
        *self.client.write().await = None;
        Ok(())
    }

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();
        let container_name = self.get_container_name();
        let password = parameters.get("password");

        let url = if let Some(pass) = password {
            format!("redis://:{pass}@{container_name}:6379")
        } else {
            format!("redis://{container_name}:6379")
        };

        env_vars.insert("REDIS_URL".to_string(), url);
        env_vars.insert("REDIS_HOST".to_string(), container_name);
        env_vars.insert("REDIS_PORT".to_string(), "6379".to_string());
        if let Some(pass) = password {
            env_vars.insert("REDIS_PASSWORD".to_string(), pass.clone());
        }

        Ok(env_vars)
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        // Generate JSON Schema from RedisInputConfig
        let schema = schemars::schema_for!(RedisInputConfig);
        serde_json::to_value(schema).ok()
    }

    fn get_runtime_env_definitions(&self) -> Vec<super::RuntimeEnvVar> {
        vec![
            super::RuntimeEnvVar {
                name: "REDIS_DATABASE".to_string(),
                description: "Redis database number for this project/environment".to_string(),
                example: "1".to_string(),
                sensitive: false,
            },
            super::RuntimeEnvVar {
                name: "REDIS_URL".to_string(),
                description: "Full Redis URL including database number".to_string(),
                example: "redis://localhost:6379/1".to_string(),
                sensitive: true, // May contain password
            },
        ]
    }
    async fn get_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let resource_name = format!("{}_{}", project_id, environment);

        // Create the database and get its number
        let db_number = self.create_database(&resource_name).await?;

        let config_guard = self.config.read().await;
        config_guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Redis not configured"))?;

        let mut env_vars = HashMap::new();
        let container_name = self.get_container_name();

        // Database number (specific to this project/environment)
        env_vars.insert("REDIS_DATABASE".to_string(), db_number.to_string());

        // Get password from service config if available
        let password = config.parameters.get("password").and_then(|v| v.as_str());

        // Connection URL with database number
        let url = if let Some(pass) = password {
            format!("redis://:{pass}@{container_name}:6379/{db_number}")
        } else {
            format!("redis://{container_name}:6379/{db_number}")
        };
        env_vars.insert("REDIS_URL".to_string(), url);

        // Individual connection parameters (same as get_docker_environment_variables)
        env_vars.insert("REDIS_HOST".to_string(), container_name);
        env_vars.insert("REDIS_PORT".to_string(), "6379".to_string());
        if let Some(pass) = password {
            env_vars.insert("REDIS_PASSWORD".to_string(), pass.to_string());
        }

        Ok(env_vars)
    }
    async fn start(&self) -> Result<()> {
        let container_name = self.get_container_name();
        info!("Starting Redis container {}", container_name);

        let containers = self
            .docker
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
                .ok_or_else(|| anyhow::anyhow!("Redis configuration not found"))?
                .clone();
            self.create_container(&self.docker, &config, &config.password)
                .await?;
        } else {
            self.docker
                .start_container(
                    &container_name,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start existing Redis container: {}", e))?;
        }

        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // Clear the connection manager
        *self.connection_manager.write().await = None;
        *self.client.write().await = None;

        // Stop the container if Docker is available
        let container_name = self.get_container_name();
        info!("Stopping Redis container {}", container_name);

        let containers = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            self.docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop Redis container: {}", e))?;
        }

        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        // First cleanup any connections
        self.cleanup().await?;

        // Then remove container and volume if Docker is available
        let container_name = self.get_container_name();
        let volume_name = format!("redis_data_{}", self.name);

        info!("Removing Redis container and volume for {}", self.name);

        // Remove container if it exists
        let containers = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.clone()],
                )])),
                ..Default::default()
            }))
            .await?;

        if !containers.is_empty() {
            // Stop container first if running
            self.docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop Redis container: {}", e))?;

            // Remove the container
            self.docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to remove Redis container: {}", e))?;
        }

        // Remove volume
        match self
            .docker
            .remove_volume(
                &volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await
        {
            Ok(_) => info!("Removed volume {}", volume_name),
            Err(e) => info!("Error removing volume {}: {}", volume_name, e),
        }

        Ok(())
    }

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();

        let host = parameters.get("host").context("Missing host parameter")?;
        let port = parameters.get("port").context("Missing port parameter")?;
        let password = parameters.get("password");

        let url = if let Some(pass) = password {
            format!("redis://:{pass}@{host}:{port}")
        } else {
            format!("redis://{host}:{port}")
        };

        env_vars.insert("REDIS_URL".to_string(), url);
        env_vars.insert("REDIS_HOST".to_string(), host.clone());
        env_vars.insert("REDIS_PORT".to_string(), port.clone());
        if let Some(pass) = password {
            env_vars.insert("REDIS_PASSWORD".to_string(), pass.clone());
        }

        Ok(env_vars)
    }

    async fn deprovision_resource(&self, project_id: &str, environment: &str) -> Result<()> {
        let resource_name = format!("{}_{}", project_id, environment);
        self.drop_database(&resource_name).await
    }

    /// Backup Redis data to S3
    async fn backup_to_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        subpath: &str,
        _subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        _service_config: ServiceConfig,
    ) -> Result<String> {
        use chrono::Utc;
        use sea_orm::*;
        use std::io::Write;

        info!("Starting Redis backup to S3");

        // Create a backup record
        let backup_record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
                service_id: Set(external_service.id),
                backup_id: Set(backup.id),
                backup_type: Set("full".to_string()),
                state: Set("running".to_string()),
                started_at: Set(Utc::now()),
                s3_location: Set("".to_string()),
                metadata: Set(serde_json::json!({
                    "service_type": "redis",
                    "service_name": self.name,
                })),
                compression_type: Set("none".to_string()),
                created_by: Set(0), // System user ID
                ..Default::default()
            },
        )
        .exec_with_returning(pool)
        .await?;

        // Get container name
        let container_name = self.get_container_name();

        // Create a temporary directory for the backup
        let temp_dir = tempfile::tempdir()?;
        let temp_path = temp_dir.path();

        // Execute BGSAVE to create a new RDB file without blocking
        self.docker
            .create_exec(
                &container_name,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec!["redis-cli", "BGSAVE"]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        // Wait a moment for BGSAVE to complete
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Copy both dump.rdb and appendonly.aof from container
        for file in &["dump.rdb", "appendonly.aof"] {
            let cat_exec = self
                .docker
                .create_exec(
                    &container_name,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(vec!["cat", &format!("/data/{}", file)]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;

            let file_path = temp_path.join(file);
            let mut temp_file = std::fs::File::create(&file_path)?;

            let output = self.docker.start_exec(&cat_exec.id, None).await?;
            if let bollard::exec::StartExecResults::Attached { output, .. } = output {
                let mut stream = output.boxed();
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(log_output) => match log_output {
                            bollard::container::LogOutput::StdOut { message }
                            | bollard::container::LogOutput::StdErr { message } => {
                                temp_file.write_all(&message)?;
                            }
                            _ => (),
                        },
                        Err(e) => {
                            error!("Error streaming backup data for {}: {}", file, e);
                            // Update backup record with error
                            let mut backup_update: temps_entities::external_service_backups::ActiveModel = backup_record.clone().into();
                            backup_update.state = Set("failed".to_string());
                            backup_update.error_message = Set(Some(e.to_string()));
                            backup_update.finished_at = Set(Some(Utc::now()));
                            temps_entities::external_service_backups::Entity::update(backup_update)
                                .exec(pool)
                                .await?;
                            return Err(anyhow::anyhow!("Failed to stream backup data: {}", e));
                        }
                    }
                }
            }
        }

        // Create a tar archive containing both files
        let tar_path = temp_path.join("redis_backup.tar");
        let tar_file = std::fs::File::create(&tar_path)?;
        let mut tar_builder = tar::Builder::new(tar_file);

        // Add both files to the tar archive
        for file in &["dump.rdb", "appendonly.aof"] {
            let file_path = temp_path.join(file);
            tar_builder.append_path_with_name(&file_path, file)?;
        }
        tar_builder.finish()?;

        // Generate backup path in S3
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_key = format!(
            "{}/redis_backup_{}.tar",
            subpath.trim_matches('/'),
            timestamp
        );

        // Upload to S3
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_path(&tar_path).await?)
            .content_type("application/x-tar")
            .send()
            .await?;

        // Get file size
        let size_bytes = std::fs::metadata(&tar_path)?.len() as i32;

        // Update backup record with success
        let mut backup_update: temps_entities::external_service_backups::ActiveModel =
            backup_record.clone().into();
        backup_update.state = Set("completed".to_string());
        backup_update.finished_at = Set(Some(Utc::now()));
        backup_update.size_bytes = Set(Some(size_bytes));
        backup_update.s3_location = Set(backup_key.clone());
        backup_update.update(pool).await?;

        info!("Redis backup completed successfully");
        Ok(backup_key)
    }

    async fn restore_from_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        _service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Starting Redis restore from S3: {}", backup_location);

        // Get the backup object from S3
        let get_obj = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(backup_location)
            .send()
            .await?;

        // Read the backup data
        let backup_data = get_obj.body.collect().await?.to_vec();

        // Get container name
        let container_name = self.get_container_name();

        self.docker
            .stop_container(&container_name, None::<StopContainerOptions>)
            .await
            .context("Failed to stop Redis container")?;

        // Create a temporary directory
        let temp_dir = tempfile::tempdir()?;
        let tar_path = temp_dir.path().join("backup.tar");

        // Write the tar file
        tokio::fs::write(&tar_path, backup_data).await?;

        // Extract the tar file
        let tar_file = std::fs::File::open(&tar_path)?;
        let mut archive = tar::Archive::new(tar_file);
        archive.unpack(temp_dir.path())?;

        // Create a new tar archive with the extracted files in the correct structure
        let mut tar = tar::Builder::new(Vec::new());
        for file in &["dump.rdb", "appendonly.aof"] {
            let file_path = temp_dir.path().join(file);
            if file_path.exists() {
                tar.append_path_with_name(&file_path, file)?;
            }
        }
        let tar_data = tar.into_inner()?;

        // Copy both files into the container's data directory
        self.docker
            .upload_to_container(
                &container_name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/data".to_string(),
                    ..Default::default()
                }),
                body_full(bytes::Bytes::from(tar_data)),
            )
            .await
            .context("Failed to upload backup files to container")?;

        // Start Redis server again
        self.docker
            .start_container(
                &container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .context("Failed to start Redis container")?;

        // Wait for container to be healthy
        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        info!("Redis restore completed successfully");
        Ok(())
    }
}
