use anyhow::{Context, Result};
use async_trait::async_trait;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::{body_full, Docker};
use futures::{StreamExt, TryStreamExt};
use schemars::JsonSchema;
use sea_orm::{prelude::*, *};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use temps_entities::external_service_backups;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info};

use crate::utils::ensure_network_exists;

use super::{ExternalService, RuntimeEnvVar, ServiceConfig, ServiceType};

/// Input configuration for creating a PostgreSQL service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "PostgreSQL Configuration",
    description = "Configuration for PostgreSQL service"
)]
pub struct PostgresInputConfig {
    /// PostgreSQL host address
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// PostgreSQL port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// PostgreSQL database name
    #[serde(default = "default_database")]
    #[schemars(example = "example_database", default = "default_database")]
    pub database: String,

    /// PostgreSQL username
    #[serde(default = "default_username")]
    #[schemars(example = "example_username", default = "default_username")]
    pub username: String,

    /// PostgreSQL password (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(with = "Option<String>", example = "example_password")]
    pub password: Option<String>,

    /// Maximum number of connections
    #[serde(default = "default_max_connections")]
    #[schemars(
        example = "example_max_connections",
        default = "default_max_connections"
    )]
    pub max_connections: u32,

    /// SSL mode (disable, allow, prefer, require)
    #[serde(default = "default_ssl_mode")]
    #[schemars(example = "example_ssl_mode", default = "default_ssl_mode_string")]
    pub ssl_mode: Option<String>,

    /// Docker image to use (defaults to postgres:17-alpine, supports timescaledb/timescaledb-ha:pg17)
    #[serde(default = "default_docker_image")]
    #[schemars(example = "example_docker_image", default = "default_docker_image")]
    pub docker_image: Option<String>,
}

/// Internal runtime configuration for PostgreSQL service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConfig {
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub max_connections: u32,
    pub ssl_mode: Option<String>,
    pub docker_image: String,
}

impl From<PostgresInputConfig> for PostgresConfig {
    fn from(input: PostgresInputConfig) -> Self {
        Self {
            host: input.host,
            port: input.port.unwrap_or_else(|| {
                find_available_port(5432)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "5432".to_string())
            }),
            database: input.database,
            username: input.username,
            password: input.password.unwrap_or_else(generate_password),
            max_connections: input.max_connections,
            ssl_mode: input.ssl_mode,
            docker_image: input
                .docker_image
                .unwrap_or_else(|| "postgres:17-alpine".to_string()),
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

fn default_database() -> String {
    "postgres".to_string()
}

fn default_username() -> String {
    "postgres".to_string()
}

fn generate_password() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

fn default_max_connections() -> u32 {
    100
}

fn default_ssl_mode() -> Option<String> {
    Some("disable".to_string())
}

fn default_ssl_mode_string() -> String {
    "disable".to_string()
}

fn default_docker_image() -> Option<String> {
    Some("postgres:17-alpine".to_string())
}

// Schema example functions
fn example_host() -> &'static str {
    "localhost"
}

fn example_port() -> &'static str {
    "5432"
}

fn example_database() -> &'static str {
    "myapp"
}

fn example_username() -> &'static str {
    "postgres"
}

fn example_password() -> &'static str {
    "your-secure-password"
}

fn example_max_connections() -> u32 {
    10
}

fn example_ssl_mode() -> &'static str {
    "disable"
}

fn example_docker_image() -> &'static str {
    "postgres:17-alpine"
}

fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn find_available_port(start_port: u16) -> Option<u16> {
    (start_port..start_port + 100).find(|&port| is_port_available(port))
}

pub struct PostgresService {
    name: String,
    config: Arc<RwLock<Option<PostgresConfig>>>,
    docker: Arc<Docker>,
}

impl PostgresService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            docker,
        }
    }

    fn get_postgres_config(&self, service_config: ServiceConfig) -> Result<PostgresConfig> {
        // Parse input config and transform to runtime config
        let input_config: PostgresInputConfig =
            serde_json::from_value(service_config.parameters)
                .map_err(|e| anyhow::anyhow!("Failed to parse PostgreSQL configuration: {}", e))?;

        Ok(PostgresConfig::from(input_config))
    }
    fn get_container_name(&self) -> String {
        format!("postgres-{}", self.name)
    }

    async fn create_container(&self, docker: &Docker, config: &PostgresConfig) -> Result<()> {
        // Pull image first
        info!("Pulling PostgreSQL image {}", config.docker_image);

        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = config.docker_image.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (config.docker_image.clone(), "latest".to_string())
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
            .await?;

        let container_name = self.get_container_name();
        let volume_name = format!("{}_data", container_name);

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
        };

        // Check if container already exists
        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![container_name.to_string()],
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
            (service_label_key, "postgres".to_string()),
            (name_label_key, self.name.to_string()),
        ]);

        let env_vars = [
            format!("POSTGRES_USER={}", config.username),
            format!("POSTGRES_PASSWORD={}", config.password),
            format!("POSTGRES_DB={}", config.database),
        ];

        let host_config = bollard::models::HostConfig {
            port_bindings: Some(HashMap::from([(
                "5432/tcp".to_string(),
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(config.port.clone()),
                }]),
            )])),
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/var/lib/postgresql/data".to_string()),
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
            image: Some(config.docker_image.clone()),
            exposed_ports: Some(HashMap::from([("5432/tcp".to_string(), HashMap::new())])),
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
                    "pg_isready -U postgres".to_string(),
                ]),
                interval: Some(1000000000), // 1 second
                timeout: Some(3000000000),  // 3 seconds
                retries: Some(3),
                start_period: Some(5000000000),   // 5 seconds
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
            .map_err(|e| anyhow::anyhow!("Failed to create PostgreSQL container: {}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start PostgreSQL container: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container.id)
            .await?;

        info!("PostgreSQL container {} created and started", container.id);
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

        Err(anyhow::anyhow!(
            "PostgreSQL container health check timed out"
        ))
    }

    async fn create_database(&self, service_config: ServiceConfig, name: &str) -> Result<()> {
        let config: PostgresConfig = self.get_postgres_config(service_config)?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.max_connections)
            .connect(&format!(
                "postgres://{}:{}@{}:{}/postgres",
                config.username, config.password, config.host, config.port
            ))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to postgres: {}", e))?;

        // Check if database exists
        let check_db = format!("SELECT 1 FROM pg_database WHERE datname = '{}'", name);
        info!("Checking if database exists: {}", check_db);
        let exists = sqlx::query(&check_db)
            .fetch_optional(&pool)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to check database existence: {}", e))?;

        if exists.is_none() {
            // Create database if it doesn't exist
            let create_db = format!("CREATE DATABASE {}", name);
            info!("Creating database sql: {}", create_db);
            sqlx::query(&create_db)
                .execute(&pool)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create database: {}", e))?;
        } else {
            info!("Database {} already exists, skipping creation", name);
        }

        Ok(())
    }

    async fn drop_database(&self, _name: &str) -> Result<()> {
        Ok(())
    }

    fn normalize_database_name(name: &str) -> String {
        let normalized = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect::<String>();

        let prefixed = if normalized.chars().next().unwrap().is_numeric() {
            format!("db_{}", normalized)
        } else {
            normalized
        };

        if prefixed.len() > 63 {
            prefixed[..63].to_string()
        } else {
            prefixed
        }
    }

    async fn restore_backup_file(
        &self,
        docker: &Docker,
        container_name: &str,
        backup_data: Vec<u8>,
    ) -> Result<()> {
        // Create a temporary file with the backup data
        // Create a temporary file for the backup data
        let temp_file = tempfile::NamedTempFile::new()?;
        tokio::fs::write(temp_file.path(), backup_data).await?;

        // Create a tar archive containing the backup file
        let mut tar = tar::Builder::new(Vec::new());
        tar.append_path_with_name(temp_file.path(), "backup.sql")?;
        let tar_data = tar.into_inner()?;
        // Copy the tar archive into the container
        docker
            .upload_to_container(
                container_name,
                Some(bollard::query_parameters::UploadToContainerOptions {
                    path: "/".to_string(),
                    ..Default::default()
                }),
                body_full(bytes::Bytes::from(tar_data)),
            )
            .await
            .context("Failed to upload backup file to container")?;

        // Execute psql to restore the backup
        let exec = docker
            .create_exec(
                container_name,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec!["psql", "-U", "postgres", "-f", "/backup.sql"]),
                    env: Some(vec!["PGPASSWORD=postgres"]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        let output = docker.start_exec(&exec.id, None).await?;
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
            while let Some(Ok(output)) = output.next().await {
                match output {
                    bollard::container::LogOutput::StdOut { message } => {
                        info!("stdout: {}", String::from_utf8_lossy(&message));
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        error!("stderr: {}", String::from_utf8_lossy(&message));
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl ExternalService for PostgresService {
    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_postgres_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

    /// Backup PostgreSQL data to S3
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
        use tempfile::NamedTempFile;

        info!("Starting PostgreSQL backup to S3");
        let metadata = serde_json::json!({
            "service_type": "postgres",
            "service_name": self.name,
        });
        // Create a backup record
        let backup_record = external_service_backups::ActiveModel {
            service_id: Set(external_service.id),
            backup_id: Set(backup.id),
            backup_type: Set("full".to_string()),
            state: Set("running".to_string()),
            started_at: Set(Utc::now()),
            s3_location: Set("".to_string()),
            metadata: Set(metadata),
            compression_type: Set("gzip".to_string()),
            created_by: Set(0), // System user ID
            ..Default::default()
        }
        .insert(pool)
        .await?;

        // Get container name
        let container_name = self.get_container_name();

        // Create a temporary file for the backup
        let mut temp_file = tempfile::NamedTempFile::new()?;

        // Execute pg_dumpall inside the container
        let exec = self
            .docker
            .create_exec(
                &container_name,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec![
                        "pg_dumpall",
                        "-U",
                        "postgres",
                        "-w",
                        "--clean",
                        "--if-exists",
                    ]),
                    env: Some(vec!["PGPASSWORD=postgres"]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        let output: bollard::exec::StartExecResults =
            self.docker.start_exec(&exec.id, None).await?;
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
                        error!("Error streaming backup data: {}", e);
                        // Update backup record with error
                        let mut backup_update: external_service_backups::ActiveModel =
                            backup_record.clone().into();
                        backup_update.state = Set("failed".to_string());
                        backup_update.error_message = Set(Some(e.to_string()));
                        backup_update.finished_at = Set(Some(Utc::now()));
                        backup_update.update(pool).await?;
                        return Err(anyhow::anyhow!("Failed to stream backup data: {}", e));
                    }
                }
            }
        }

        // Generate backup path in S3
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_key = format!(
            "{}/postgres_backup_{}.sql",
            subpath.trim_matches('/'),
            timestamp
        );

        // Compress the backup
        let mut compressed_file = NamedTempFile::new()?;
        let mut encoder =
            flate2::write::GzEncoder::new(&mut compressed_file, flate2::Compression::default());
        std::io::copy(&mut std::fs::File::open(temp_file.path())?, &mut encoder)?;
        encoder.finish()?;

        // Get file size before compression
        let size_bytes = temp_file.as_file().metadata()?.len() as i32;

        // Upload to S3
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_path(compressed_file.path()).await?)
            .content_type("application/gzip")
            .content_encoding("gzip")
            .send()
            .await?;

        // Update backup record with success
        let mut backup_update: external_service_backups::ActiveModel = backup_record.clone().into();
        backup_update.state = Set("completed".to_string());
        backup_update.finished_at = Set(Some(Utc::now()));
        backup_update.size_bytes = Set(Some(size_bytes));
        backup_update.s3_location = Set(backup_key.clone());
        backup_update.update(pool).await?;

        info!("PostgreSQL backup completed successfully");
        Ok(backup_key)
    }
    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!("Initializing PostgreSQL service {:?}", config);

        // Parse input config and transform to runtime config
        let postgres_config = self.get_postgres_config(config)?;

        // Store runtime config
        *self.config.write().await = Some(postgres_config.clone());

        // Create Docker container
        self.create_container(&self.docker, &postgres_config)
            .await?;

        // Serialize the full runtime config to save to database
        // This ensures auto-generated values (password, port) are persisted
        let runtime_config_json = serde_json::to_value(&postgres_config)
            .context("Failed to serialize PostgreSQL runtime config")?;

        let runtime_config_map = runtime_config_json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Runtime config is not an object"))?;

        let mut inferred_params = HashMap::new();
        for (key, value) in runtime_config_map {
            if let Some(str_value) = value.as_str() {
                inferred_params.insert(key.clone(), str_value.to_string());
            } else if let Some(num_value) = value.as_u64() {
                inferred_params.insert(key.clone(), num_value.to_string());
            }
        }

        Ok(inferred_params)
    }

    async fn health_check(&self) -> Result<bool> {
        // let pool = self.get_pool().await?;
        // let result = sqlx::query("SELECT 1").fetch_one(&pool).await.is_ok();
        Ok(true)
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Postgres
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
            Some(cfg) => Ok(format!(
                "postgres://{}:***@{}:{}/{}",
                cfg.username, cfg.host, cfg.port, cfg.database
            )),
            None => Err(anyhow::anyhow!("PostgreSQL not configured")),
        }
    }

    fn get_runtime_env_definitions(&self) -> Vec<RuntimeEnvVar> {
        vec![
            RuntimeEnvVar {
                name: "POSTGRES_DATABASE".to_string(),
                description: "Database name specific to this project/environment".to_string(),
                example: "project_123_production".to_string(),
                sensitive: false,
            },
            RuntimeEnvVar {
                name: "POSTGRES_URL".to_string(),
                description: "Full connection URL including project-specific database".to_string(),
                example: "postgresql://user:pass@localhost:5432/project_123_production".to_string(),
                sensitive: true, // Contains password
            },
        ]
    }
    async fn get_runtime_env_vars(
        &self,
        service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let resource_name = format!("{}_{}", project_id, environment);
        let resource_name = Self::normalize_database_name(&resource_name);

        // Create the database
        self.create_database(service_config.clone(), &resource_name)
            .await?;
        let config: PostgresConfig = self.get_postgres_config(service_config)?;
        let mut env_vars = HashMap::new();
        let container_name = self.get_container_name();

        // Database-specific variable
        env_vars.insert("POSTGRES_DATABASE".to_string(), resource_name.clone());

        // Connection URL
        env_vars.insert(
            "POSTGRES_URL".to_string(),
            format!(
                "postgresql://{}:{}@{}:{}/{}",
                config.username, config.password, container_name, 5432, resource_name
            ),
        );

        // Individual connection parameters (same as get_docker_environment_variables)
        env_vars.insert("POSTGRES_HOST".to_string(), container_name);
        env_vars.insert("POSTGRES_PORT".to_string(), "5432".to_string());
        env_vars.insert("POSTGRES_NAME".to_string(), resource_name.clone());
        env_vars.insert("POSTGRES_USER".to_string(), config.username.clone());
        env_vars.insert("POSTGRES_PASSWORD".to_string(), config.password.clone());

        Ok(env_vars)
    }
    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();
        let container_name = self.get_container_name();

        let username = parameters
            .get("username")
            .context("Missing username parameter")?;
        let password = parameters
            .get("password")
            .context("Missing password parameter")?;
        let database = parameters
            .get("database")
            .context("Missing database parameter")?;

        let url = format!(
            "postgresql://{}:{}@{}:{}/{}",
            username, password, container_name, 5432, database
        );

        env_vars.insert("POSTGRES_URL".to_string(), url);
        env_vars.insert("POSTGRES_HOST".to_string(), container_name);
        env_vars.insert("POSTGRES_PORT".to_string(), "5432".to_string());
        env_vars.insert("POSTGRES_NAME".to_string(), database.clone());
        env_vars.insert("POSTGRES_USER".to_string(), username.clone());
        env_vars.insert("POSTGRES_PASSWORD".to_string(), password.clone());

        Ok(env_vars)
    }
    async fn cleanup(&self) -> Result<()> {
        Ok(())
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        // Generate JSON Schema from PostgresInputConfig
        let schema = schemars::schema_for!(PostgresInputConfig);
        serde_json::to_value(schema).ok()
    }

    async fn start(&self) -> Result<()> {
        let container_name = self.get_container_name();
        info!("Starting PostgreSQL container {}", container_name);

        // Check if container exists and get its status
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
            // Container doesn't exist, create        and start it
            let config = self
                .config
                .read()
                .await
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("PostgreSQL configuration not found"))?
                .clone();
            self.create_container(&self.docker, &config).await?;
        } else {
            // Container exists, just start it if it's not running
            self.docker
                .start_container(
                    &container_name,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .context("Failed to start existing PostgreSQL container")?;
        }

        // Wait for container to be healthy
        self.wait_for_container_health(&self.docker, &container_name)
            .await?;

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // Stop the container if Docker is available
        let container_name = self.get_container_name();

        // Check if container exists before attempting to stop
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
                .stop_container(
                    &container_name,
                    None::<bollard::query_parameters::StopContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop PostgreSQL container: {:?}", e))?;
        }

        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        // First cleanup any connections
        self.cleanup().await?;

        // Then remove container and volume if Docker is available
        let container_name = self.get_container_name();
        let volume_name = format!("{}_data", container_name);

        info!("Removing PostgreSQL container and volume for {}", self.name);

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
                .context("Failed to stop PostgreSQL container")?;

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
                .context("Failed to remove PostgreSQL container")?;
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
        let database = parameters
            .get("database")
            .context("Missing database parameter")?;
        let username = parameters
            .get("username")
            .context("Missing username parameter")?;
        let password = parameters
            .get("password")
            .context("Missing password parameter")?;

        let url = format!(
            "postgresql://{}:{}@{}:{}/{}",
            username, password, host, port, database
        );

        env_vars.insert("POSTGRES_URL".to_string(), url);
        env_vars.insert("POSTGRES_HOST".to_string(), host.clone());
        env_vars.insert("POSTGRES_PORT".to_string(), port.clone());
        env_vars.insert("POSTGRES_NAME".to_string(), database.clone());
        env_vars.insert("POSTGRES_USER".to_string(), username.clone());
        env_vars.insert("POSTGRES_PASSWORD".to_string(), password.clone());

        Ok(env_vars)
    }

    async fn deprovision_resource(&self, project_id: &str, environment: &str) -> Result<()> {
        let resource_name = format!("{}_{}", project_id, environment);
        self.drop_database(&resource_name).await
    }

    async fn restore_from_s3(
        &self,
        s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        _service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Starting PostgreSQL restore from S3: {}", backup_location);

        // Get the backup object from S3
        let get_obj = s3_client
            .get_object()
            .bucket(&s3_source.bucket_name)
            .key(backup_location)
            .send()
            .await?;

        // Read the backup data
        let backup_data = get_obj.body.collect().await?.to_vec();

        // Decompress if needed (assuming gzip compression)
        let mut decoder = flate2::read::GzDecoder::new(&backup_data[..]);
        let mut decompressed_data = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed_data)?;

        // Get container name
        let container_name = self.get_container_name();

        // Restore the backup using Docker
        self.restore_backup_file(&self.docker, &container_name, decompressed_data)
            .await?;

        info!("PostgreSQL restore completed successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_postgres_input_config_default_values() {
        let config = PostgresInputConfig {
            host: default_host(),
            port: None,
            database: default_database(),
            username: default_username(),
            password: None,
            max_connections: default_max_connections(),
            ssl_mode: default_ssl_mode(),
            docker_image: None,
        };

        let runtime_config: PostgresConfig = config.into();

        assert_eq!(runtime_config.host, "localhost");
        assert_eq!(runtime_config.database, "postgres");
        assert_eq!(runtime_config.username, "postgres");
        assert_eq!(runtime_config.max_connections, 100);
        assert_eq!(runtime_config.docker_image, "postgres:17-alpine");
        assert!(runtime_config.password.len() >= 16); // Auto-generated password
    }

    #[test]
    fn test_postgres_input_config_custom_docker_image() {
        let config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "mydb".to_string(),
            username: "myuser".to_string(),
            password: Some("mypass".to_string()),
            max_connections: 50,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("timescale/timescaledb-ha:pg17".to_string()),
        };

        let runtime_config: PostgresConfig = config.into();

        assert_eq!(runtime_config.docker_image, "timescale/timescaledb-ha:pg17");
    }
}
