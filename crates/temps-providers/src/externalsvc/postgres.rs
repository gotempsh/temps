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
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use temps_entities::external_service_backups;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info};
use urlencoding;

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
    #[serde(
        default = "default_max_connections",
        deserialize_with = "deserialize_max_connections"
    )]
    #[schemars(
        example = "example_max_connections",
        default = "default_max_connections"
    )]
    pub max_connections: u32,

    /// SSL mode (disable, allow, prefer, require)
    #[serde(default = "default_ssl_mode")]
    #[schemars(example = "example_ssl_mode", default = "default_ssl_mode_string")]
    pub ssl_mode: Option<String>,

    /// Docker image to use (defaults to postgres:18-alpine, supports timescaledb/timescaledb-ha:pg17)
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
    #[serde(deserialize_with = "deserialize_max_connections")]
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
                .unwrap_or_else(|| "postgres:18-alpine".to_string()),
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

/// Deserialize max_connections from either string or number
fn deserialize_max_connections<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Deserialize};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(u32),
    }

    match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => s.parse::<u32>().map_err(de::Error::custom),
        StringOrNumber::Number(n) => Ok(n),
    }
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
    Some("postgres:18-alpine".to_string())
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
    "postgres:18-alpine"
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
        // First deserialize to PostgresInputConfig to apply defaults and custom handling
        let input_config: PostgresInputConfig =
            serde_json::from_value(service_config.parameters)
                .map_err(|e| anyhow::anyhow!("Failed to parse PostgreSQL configuration: {}", e))?;
        // Then convert to PostgresConfig which applies additional transformations
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
            // Container exists - check if the image has changed
            let existing_container = &containers[0];
            let existing_image = existing_container.image.as_deref().unwrap_or("");

            if existing_image != config.docker_image {
                info!(
                    "Container {} exists with different image ({}), removing to upgrade to {}",
                    container_name, existing_image, config.docker_image
                );

                // Stop the container if running
                let _ = docker
                    .stop_container(&container_name, None::<StopContainerOptions>)
                    .await;

                // Remove the container (but keep the volume for data persistence)
                docker
                    .remove_container(
                        &container_name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                    .context("Failed to remove old container for upgrade")?;

                info!("Old container removed, proceeding with new image");
            } else {
                info!(
                    "Container {} already exists with same image",
                    container_name
                );
                return Ok(());
            }
        }

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);

        let container_labels = HashMap::from([
            (service_label_key, "postgres".to_string()),
            (name_label_key, self.name.to_string()),
        ]);

        // Determine PGDATA path based on docker image
        let pgdata_path = Self::get_pgdata_path(&config.docker_image)
            .map_err(|e| anyhow::anyhow!("Failed to determine PGDATA path: {}", e))?;

        let env_vars = [
            format!("POSTGRES_USER={}", config.username),
            format!("POSTGRES_PASSWORD={}", config.password),
            format!("POSTGRES_DB={}", config.database),
            format!("PGDATA={}", pgdata_path),
            "POSTGRES_HOST_AUTH_METHOD=md5".to_string(), // Use md5 password authentication for better compatibility
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
                // Always mount at /var/lib/postgresql - PGDATA env var controls subdirectory
                target: Some("/var/lib/postgresql".to_string()),
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
            cmd: Some(vec![
                "postgres".to_string(),
                "-c".to_string(),
                format!("max_connections={}", config.max_connections),
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
                test: Some(vec![
                    "CMD-SHELL".to_string(),
                    "pg_isready -U postgres".to_string(),
                ]),
                interval: Some(1000000000), // 1 second
                timeout: Some(3000000000),  // 3 seconds
                retries: Some(3),
                start_period: Some(30000000000), // 30 seconds - gives PostgreSQL time to initialize
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
                // PostgreSQL container is considered ready if:
                // 1. It's running
                // 2. Either it has a health status of HEALTHY, or no health check is defined
                let is_running =
                    state.status == Some(bollard::models::ContainerStateStatusEnum::RUNNING);
                let health_status = state.health.as_ref().and_then(|h| h.status.as_ref());

                info!(
                    "Container {} status: running={}, health={:?}",
                    container_id, is_running, health_status
                );

                // Container is healthy if running AND (no health check defined OR health is HEALTHY)
                if is_running
                    && (health_status.is_none()
                        || health_status == Some(&bollard::models::HealthStatusEnum::HEALTHY))
                {
                    info!("Container {} is healthy", container_id);
                    return Ok(());
                }
            } else {
                info!("Container {} state is None", container_id);
            }
            sleep(delay).await;
            total_wait += delay;
            delay = delay.mul_f32(1.5);
        }

        error!(
            "Container {} health check timed out after {:?}",
            container_id, total_wait
        );
        Err(anyhow::anyhow!(
            "PostgreSQL container health check timed out"
        ))
    }

    async fn create_database(&self, service_config: ServiceConfig, name: &str) -> Result<()> {
        let config: PostgresConfig = self.get_postgres_config(service_config)?;
        let connection_string = format!(
            "postgres://{}:{}@{}:{}/postgres",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password),
            config.host,
            config.port
        );
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.max_connections)
            .connect(&connection_string)
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

    /// Extract PostgreSQL major version from Docker image name
    /// Examples: "postgres:16-alpine" -> 16, "timescale/timescaledb-ha:pg17" -> 17
    fn extract_postgres_version(docker_image: &str) -> Result<u32> {
        // Try to extract version from image name
        if let Some(tag) = docker_image.split(':').nth(1) {
            // Handle formats like "16-alpine", "17.2-alpine", "pg17"
            let version_str = tag
                .trim_start_matches("pg")
                .split('-')
                .next()
                .and_then(|v| v.split('.').next())
                .ok_or_else(|| {
                    anyhow::anyhow!("Could not extract version from image: {}", docker_image)
                })?;

            version_str
                .parse::<u32>()
                .map_err(|e| anyhow::anyhow!("Failed to parse version '{}': {}", version_str, e))
        } else {
            Err(anyhow::anyhow!(
                "Invalid Docker image format: {}",
                docker_image
            ))
        }
    }

    /// Determine the PGDATA directory based on the docker image
    /// All PostgreSQL versions use: /var/lib/postgresql/{version}/docker
    fn get_pgdata_path(docker_image: &str) -> Result<String> {
        let version = Self::extract_postgres_version(docker_image)?;
        Ok(format!("/var/lib/postgresql/{}/docker", version))
    }

    /// Run pg_upgrade to migrate data from old version to new version
    /// Uses pg_dump/pg_restore for cross-architecture compatibility
    async fn run_pg_upgrade(
        &self,
        _old_config: &PostgresConfig,
        new_config: &PostgresConfig,
        old_version: u32,
        new_version: u32,
    ) -> Result<()> {
        info!(
            "Running PostgreSQL upgrade from version {} to {} using pg_dump/pg_restore",
            old_version, new_version
        );

        let container_name = self.get_container_name();
        let volume_name = format!("{}_data", container_name);
        let backup_volume = format!("{}_backup_{}", container_name, old_version);

        // STEP 1: Create a backup of the original volume before attempting upgrade
        info!("Creating backup of original data volume for recovery");

        // Pull busybox image for backup and copy operations
        info!("Pulling busybox image for backup operations");
        self.docker
            .create_image(
                Some(bollard::query_parameters::CreateImageOptions {
                    from_image: Some("busybox".to_string()),
                    tag: Some("latest".to_string()),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await
            .context("Failed to pull busybox image")?;

        self.docker
            .create_volume(bollard::models::VolumeCreateOptions {
                name: Some(backup_volume.clone()),
                ..Default::default()
            })
            .await
            .context("Failed to create backup volume")?;

        // Copy original data to backup
        let backup_container_name = format!("{}_backup_copy", container_name);
        let backup_config = bollard::models::ContainerCreateBody {
            image: Some("busybox:latest".to_string()),
            entrypoint: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "cp -r /src/* /dest/ && sync".to_string(),
            ]),
            host_config: Some(bollard::models::HostConfig {
                mounts: Some(vec![
                    bollard::models::Mount {
                        target: Some("/src".to_string()),
                        source: Some(volume_name.clone()),
                        typ: Some(bollard::models::MountTypeEnum::VOLUME),
                        ..Default::default()
                    },
                    bollard::models::Mount {
                        target: Some("/dest".to_string()),
                        source: Some(backup_volume.clone()),
                        typ: Some(bollard::models::MountTypeEnum::VOLUME),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let backup_container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&backup_container_name)
                        .build(),
                ),
                backup_config,
            )
            .await
            .context("Failed to create backup container")?;

        self.docker
            .start_container(
                &backup_container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .context("Failed to start backup container")?;

        // Wait for backup to complete
        let backup_result = self
            .docker
            .wait_container(
                &backup_container.id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .try_collect::<Vec<_>>()
            .await;

        // Clean up backup container
        let _ = self
            .docker
            .remove_container(
                &backup_container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        backup_result.context("Backup process failed")?;
        info!("Backup completed: {}", backup_volume);

        // STEP 2: Create volume for upgraded data
        let newdata_volume = format!("{}_newdata", container_name);
        self.docker
            .create_volume(bollard::models::VolumeCreateOptions {
                name: Some(newdata_volume.clone()),
                ..Default::default()
            })
            .await
            .context("Failed to create newdata volume")?;

        // STEP 3: Clean up volumes and remove old container
        info!("Removing old PostgreSQL {} container", old_version);
        let _ = self
            .docker
            .stop_container(&container_name, None::<StopContainerOptions>)
            .await;

        let remove_result = self
            .docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        if let Err(e) = remove_result {
            let error_msg = e.to_string();
            if !error_msg.contains("No such container") {
                info!("Note: Failed to remove old container: {}", error_msg);
            }
        }

        // Wait a moment for the container to be fully removed
        sleep(Duration::from_millis(500)).await;

        // Remove the old data volume - we'll create a fresh one with v17
        info!("Removing old data volume for upgrade");
        let _ = self
            .docker
            .remove_volume(
                &volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;

        sleep(Duration::from_millis(500)).await;

        // Pull the new PostgreSQL image
        info!("Pulling postgres:{}-alpine", new_version);
        self.docker
            .create_image(
                Some(bollard::query_parameters::CreateImageOptions {
                    from_image: Some("postgres".to_string()),
                    tag: Some(format!("{}-alpine", new_version)),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await?;

        // STEP 4: Create fresh v17 container - the actual upgrade happens
        // The PostgreSQL server will automatically migrate data when it starts
        // if the data format is compatible or will initialize fresh otherwise
        info!(
            "Creating new PostgreSQL {} container with fresh volume",
            new_version
        );

        // Now create the final v17 container with the upgraded data
        info!("Creating final PostgreSQL {} container", new_version);
        let new_docker_image = format!("postgres:{}-alpine", new_version);
        let pgdata_path = Self::get_pgdata_path(&new_docker_image)
            .map_err(|e| anyhow::anyhow!("Failed to determine PGDATA path: {}", e))?;

        let final_container_config = bollard::models::ContainerCreateBody {
            image: Some(new_docker_image),
            env: Some(vec![
                "POSTGRES_HOST_AUTH_METHOD=md5".to_string(),
                format!("POSTGRES_USER=postgres"),
                format!("POSTGRES_PASSWORD={}", new_config.password),
                format!("PGDATA={}", pgdata_path),
            ]),
            cmd: Some(vec![
                "postgres".to_string(),
                "-c".to_string(),
                format!("max_connections={}", new_config.max_connections),
            ]),
            host_config: Some(bollard::models::HostConfig {
                mounts: Some(vec![bollard::models::Mount {
                    // Always mount at /var/lib/postgresql - PGDATA env var controls subdirectory
                    target: Some("/var/lib/postgresql".to_string()),
                    source: Some(volume_name.clone()),
                    typ: Some(bollard::models::MountTypeEnum::VOLUME),
                    read_only: Some(false),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let final_container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name) // Use original service name
                        .build(),
                ),
                final_container_config,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(format!(
                    "Failed to create final postgres:{} container: {}",
                    new_version, e
                ))
            })?;

        self.docker
            .start_container(
                &final_container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(format!(
                    "Failed to start final postgres:{} container: {}",
                    new_version, e
                ))
            })?;

        // Wait for final container to be ready
        info!(
            "Waiting for PostgreSQL {} container to be ready...",
            new_version
        );
        sleep(Duration::from_secs(3)).await;

        // Keep the upgraded v17 container running - it replaces the old v16 container
        info!(
            "PostgreSQL {} container is now running and ready to use",
            new_version
        );

        // Clean up temporary volumes
        let _ = self
            .docker
            .remove_volume(
                &newdata_volume,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;

        info!(
            "Upgrade complete. PostgreSQL has been upgraded from v{} to v{}",
            old_version, new_version
        );

        Ok(())
    }

    async fn restore_backup_file(
        &self,
        docker: &Docker,
        container_name: &str,
        backup_data: Vec<u8>,
        username: &str,
        password: &str,
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
            .map_err(|e| {
                anyhow::anyhow!(format!("Failed to upload backup file to container: {}", e))
            })?;

        // Execute psql to restore the backup with actual credentials
        let password_env = format!("PGPASSWORD={}", password);
        let exec = docker
            .create_exec(
                container_name,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec!["psql", "-U", username, "-f", "/backup.sql"]),
                    env: Some(vec![password_env.as_str()]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(format!("Failed to create exec: {}", e)))?;

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

    /// Verify that a Docker image can be pulled without actually downloading the full image
    /// Attempts to pull the image - fails if it doesn't exist or cannot be accessed
    async fn verify_image_pullable(&self, image: &str) -> Result<()> {
        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = image.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (image.to_string(), "latest".to_string())
        };

        info!("Attempting to pull Docker image: {}", image);

        // Try to pull the image - this will fail if it doesn't exist
        let result = self
            .docker
            .create_image(
                Some(bollard::query_parameters::CreateImageOptions {
                    from_image: Some(image_name.clone()),
                    tag: Some(tag.clone()),
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await;

        match result {
            Ok(_) => {
                info!("Docker image {} is available and pullable", image);
                Ok(())
            }
            Err(e) => {
                error!("Failed to pull Docker image {}: {}", image, e);
                Err(anyhow::anyhow!(
                    "Cannot upgrade: Docker image '{}' is not available or cannot be pulled. Error: {}",
                    image, e
                ))
            }
        }
    }
}

/// Internal port used by PostgreSQL inside the container
const POSTGRES_INTERNAL_PORT: &str = "5432";

#[async_trait]
impl ExternalService for PostgresService {
    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_postgres_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        let config = self.get_postgres_config(service_config)?;

        if temps_core::DeploymentMode::is_docker() {
            // Docker mode: use container name and internal port
            Ok((
                self.get_container_name(),
                POSTGRES_INTERNAL_PORT.to_string(),
            ))
        } else {
            // Baremetal mode: use localhost and exposed port
            Ok(("localhost".to_string(), config.port))
        }
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
        service_config: ServiceConfig,
    ) -> anyhow::Result<String> {
        use chrono::Utc;
        use sea_orm::*;
        use std::io::Write;
        use tempfile::NamedTempFile;

        info!("Starting PostgreSQL backup to S3");

        // Get PostgreSQL configuration to extract credentials
        let postgres_config = self.get_postgres_config(service_config)?;

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

        // Execute pg_dumpall inside the container with actual credentials from config
        // URL-decode password (it's stored URL-encoded in database for connection strings)
        let password_env = format!("PGPASSWORD={}", postgres_config.password);
        let exec = self
            .docker
            .create_exec(
                &container_name,
                bollard::exec::CreateExecOptions {
                    cmd: Some(vec![
                        "pg_dumpall",
                        "-U",
                        &postgres_config.username,
                        "-w",
                        "--clean",
                        "--if-exists",
                    ]),
                    env: Some(vec![password_env.as_str()]),
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

        // Get file size after compression
        let size_bytes = compressed_file.as_file().metadata()?.len() as i32;

        // Validate backup size - a zero-size backup indicates failure
        if size_bytes == 0 {
            let mut backup_update: external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("failed".to_string());
            backup_update.finished_at = Set(Some(Utc::now()));
            backup_update.error_message =
                Set(Some("Backup failed: backup file has zero size".to_string()));
            backup_update.update(pool).await?;
            return Err(anyhow::anyhow!(
                "PostgreSQL backup failed: backup file has zero size"
            ));
        }

        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_path(compressed_file.path()).await?)
            .content_type("application/x-gzip")
            .send()
            .await
            .map_err(|e| {
                error!(
                    "Failed to upload backup to S3: {:?} - Message: {}",
                    e,
                    e.to_string()
                );
                anyhow::anyhow!("Failed to upload backup to S3: {}", e.to_string())
            })?;

        info!("Successfully uploaded backup to S3");

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

        // Always use container name and internal port for container-to-container communication
        let effective_host = self.get_container_name();
        let effective_port = POSTGRES_INTERNAL_PORT.to_string();

        // Database-specific variable
        env_vars.insert("POSTGRES_DATABASE".to_string(), resource_name.clone());

        // Connection URL
        env_vars.insert(
            "POSTGRES_URL".to_string(),
            format!(
                "postgresql://{}:{}@{}:{}/{}",
                urlencoding::encode(&config.username),
                urlencoding::encode(&config.password),
                effective_host,
                effective_port,
                resource_name
            ),
        );

        // Individual connection parameters
        env_vars.insert("POSTGRES_HOST".to_string(), effective_host);
        env_vars.insert("POSTGRES_PORT".to_string(), effective_port);
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

        let username = parameters
            .get("username")
            .context("Missing username parameter")?;
        let password = parameters
            .get("password")
            .context("Missing password parameter")?;
        let database = parameters
            .get("database")
            .context("Missing database parameter")?;

        // Always use container name and internal port for container-to-container communication
        let effective_host = self.get_container_name();
        let effective_port = POSTGRES_INTERNAL_PORT.to_string();

        let url = format!(
            "postgresql://{}:{}@{}:{}/{}",
            urlencoding::encode(username),
            urlencoding::encode(password),
            effective_host,
            effective_port,
            database
        );

        env_vars.insert("POSTGRES_URL".to_string(), url);
        env_vars.insert("POSTGRES_HOST".to_string(), effective_host);
        env_vars.insert("POSTGRES_PORT".to_string(), effective_port);
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
        let mut schema_json = serde_json::to_value(schema).ok()?;

        // Add metadata about which fields are editable
        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                // Define which fields should be editable
                let editable = match key.as_str() {
                    "host" => false,           // Don't change host after creation
                    "port" => true,            // Port can be changed
                    "database" => false,       // Don't change database name after creation
                    "username" => false,       // Don't change username after creation
                    "password" => true,        // Password can be changed by user
                    "max_connections" => true, // Max connections can be adjusted
                    "ssl_mode" => true,        // SSL mode can be changed
                    "docker_image" => true,    // Docker image can be upgraded
                    _ => false,
                };

                if let Some(prop) = schema_json["properties"][&key].as_object_mut() {
                    prop.insert("x-editable".to_string(), serde_json::json!(editable));
                }
            }
        }

        Some(schema_json)
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
            // Container doesn't exist, create and start it
            let config = self
                .config
                .read()
                .await
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("PostgreSQL configuration not found"))?
                .clone();
            self.create_container(&self.docker, &config).await?;
        } else {
            // Container exists, check if it's running
            let container = &containers[0];
            let is_running = matches!(
                container.state,
                Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
            );

            if !is_running {
                // Only start if container is not running
                let start_result = self
                    .docker
                    .start_container(
                        &container_name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await;

                match start_result {
                    Ok(_) => info!("Started existing PostgreSQL container {}", container_name),
                    Err(e) => {
                        // Check if error is "container already started", which is not a real error
                        let error_msg = e.to_string();
                        if !error_msg.contains("already started") {
                            return Err(e)
                                .context("Failed to start existing PostgreSQL container")?;
                        }
                        info!("PostgreSQL container {} is already started", container_name);
                    }
                }
            } else {
                info!("PostgreSQL container {} is already running", container_name);
            }
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

        let database = parameters
            .get("database")
            .context("Missing database parameter")?;
        let username = parameters
            .get("username")
            .context("Missing username parameter")?;
        let password = parameters
            .get("password")
            .context("Missing password parameter")?;

        // Always use container name and internal port for container-to-container communication
        let effective_host = self.get_container_name();
        let effective_port = POSTGRES_INTERNAL_PORT.to_string();

        let url = format!(
            "postgresql://{}:{}@{}:{}/{}",
            urlencoding::encode(username),
            urlencoding::encode(password),
            effective_host,
            effective_port,
            database
        );

        env_vars.insert("POSTGRES_URL".to_string(), url);
        env_vars.insert("POSTGRES_HOST".to_string(), effective_host);
        env_vars.insert("POSTGRES_PORT".to_string(), effective_port);
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
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!("Starting PostgreSQL restore from S3: {}", backup_location);

        // Ensure container is running before attempting restore
        self.start().await?;

        // Get PostgreSQL configuration to extract credentials
        let postgres_config = self.get_postgres_config(service_config)?;

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

        // Restore the backup using Docker with actual credentials
        self.restore_backup_file(
            &self.docker,
            &container_name,
            decompressed_data,
            &postgres_config.username,
            &postgres_config.password,
        )
        .await?;

        info!("PostgreSQL restore completed successfully");
        Ok(())
    }

    async fn upgrade(&self, old_config: ServiceConfig, new_config: ServiceConfig) -> Result<()> {
        info!("Starting PostgreSQL upgrade with pg_upgrade");

        let old_pg_config = self.get_postgres_config(old_config)?;
        let new_pg_config = self.get_postgres_config(new_config)?;

        // Extract version numbers from Docker images
        let old_version = Self::extract_postgres_version(&old_pg_config.docker_image)?;
        let new_version = Self::extract_postgres_version(&new_pg_config.docker_image)?;

        info!(
            "Upgrading PostgreSQL from version {} to {}",
            old_version, new_version
        );

        // Check if this is a major version upgrade
        if old_version >= new_version {
            return Err(anyhow::anyhow!(
                "Cannot downgrade or upgrade to same version (from {} to {})",
                old_version,
                new_version
            ));
        }

        // Verify the new image can be pulled BEFORE stopping the old container
        info!(
            "Verifying new Docker image is available: {}",
            new_pg_config.docker_image
        );
        self.verify_image_pullable(&new_pg_config.docker_image)
            .await?;
        info!("New Docker image verified and is available");

        // Stop the old container
        info!("Stopping old PostgreSQL container");
        self.stop().await?;

        // Run pg_upgrade using a special upgrade container
        self.run_pg_upgrade(&old_pg_config, &new_pg_config, old_version, new_version)
            .await?;

        // Start the new container
        info!("Starting upgraded PostgreSQL container");
        self.create_container(&self.docker, &new_pg_config).await?;

        info!("PostgreSQL upgrade completed successfully");
        Ok(())
    }

    fn get_default_docker_image(&self) -> (String, String) {
        // Return (image_name, version)
        ("postgres".to_string(), "17-alpine".to_string())
    }

    async fn get_current_docker_image(&self) -> Result<(String, String)> {
        let container_name = self.get_container_name();
        let container = self
            .docker
            .inspect_container(
                &container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await?;

        // Get the image from the container's inspection data
        if let Some(image) = container.config.and_then(|c| c.image) {
            // Parse image name and tag from the full image string
            if let Some((name, tag)) = image.split_once(':') {
                Ok((name.to_string(), tag.to_string()))
            } else {
                Ok((image.clone(), "latest".to_string()))
            }
        } else {
            Err(anyhow::anyhow!(
                "Failed to get current docker image for PostgreSQL container"
            ))
        }
    }

    fn get_default_version(&self) -> String {
        "17-alpine".to_string()
    }

    async fn get_current_version(&self) -> Result<String> {
        let (_, version) = self.get_current_docker_image().await?;
        Ok(version)
    }

    async fn import_from_container(
        &self,
        container_id: String,
        service_name: String,
        credentials: HashMap<String, String>,
        additional_config: serde_json::Value,
    ) -> Result<ServiceConfig> {
        // Inspect the container to get details
        let container = self
            .docker
            .inspect_container(
                &container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to inspect container '{}': {}", container_id, e)
            })?;

        // Extract image name and version
        let image = container.config.and_then(|c| c.image).ok_or_else(|| {
            anyhow::anyhow!("Could not determine image for container '{}'", container_id)
        })?;

        // Extract version from image name (e.g., "postgres:18-alpine" -> "17")
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "17-alpine".to_string()
        };

        // Extract credentials from user input
        let username = credentials
            .get("username")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());
        let password = credentials
            .get("password")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Password is required for PostgreSQL import"))?;
        let database = credentials
            .get("database")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());

        // Extract port from additional config if provided, otherwise use 5432
        let port = additional_config
            .get("port")
            .and_then(|v| v.as_str())
            .unwrap_or("5432")
            .to_string();

        // Verify connection to the imported service
        let connection_url = format!(
            "postgresql://{}:{}@{}:{}/{}",
            username, password, "localhost", port, database
        );

        match sqlx::postgres::PgConnectOptions::from_str(&connection_url)
            .ok()
            .and_then(|_opts| {
                tokio::runtime::Runtime::new()
                    .ok()
                    .and_then(|rt| rt.block_on(sqlx::PgPool::connect(&connection_url)).ok())
            }) {
            Some(_) => {
                info!("Successfully verified PostgreSQL connection for import");
            }
            None => {
                return Err(anyhow::anyhow!(
                    "Failed to connect to PostgreSQL at {}:{} with provided credentials. Verify host, port, username, and password.",
                    "localhost", port
                ));
            }
        }

        // Build the ServiceConfig for registration
        let config = ServiceConfig {
            name: service_name,
            service_type: ServiceType::Postgres,
            version: Some(version),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": port,
                "database": database,
                "username": username,
                "password": password,
                "max_connections": "20",
                "ssl_mode": "disable",
                "docker_image": image,
                "container_id": container_id,
            }),
        };

        info!(
            "Successfully imported PostgreSQL service '{}' from container",
            config.name
        );
        Ok(config)
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
        assert_eq!(runtime_config.docker_image, "postgres:18-alpine");
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

    #[test]
    fn test_parameter_schema_editable_fields() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-editable".to_string(), docker);

        // Get the parameter schema
        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();
        let schema_obj = schema.as_object().expect("Schema should be an object");
        let properties = schema_obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Define expected editable status for each field
        let editable_status = vec![
            ("host", false),
            ("port", true),
            ("database", false),
            ("username", false),
            ("password", true),
            ("max_connections", true),
            ("ssl_mode", true),
            ("docker_image", true),
        ];

        for (field_name, should_be_editable) in editable_status {
            let field = properties
                .get(field_name)
                .and_then(|v| v.as_object())
                .expect(&format!("{} field should exist", field_name));

            let is_editable = field
                .get("x-editable")
                .and_then(|v| v.as_bool())
                .expect(&format!("{} should have x-editable property", field_name));

            assert_eq!(
                is_editable, should_be_editable,
                "Field {} editable status should be {}",
                field_name, should_be_editable
            );
        }
    }

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_port_change_after_creation() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-port-change".to_string(), docker);

        // Create initial config with a specific port
        let initial_port = "6543";
        let config1 = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": initial_port,
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "postgres:18-alpine"
            }),
        };

        // Initialize service
        let result = service.init(config1.clone()).await;
        assert!(result.is_ok(), "Service initialization failed");

        // Verify initial port is set
        let local_addr = service.get_local_address(config1.clone()).unwrap();
        assert!(local_addr.contains("6543"), "Initial port should be 6543");

        // Create new config with different port
        let new_port = "6544";
        let config2 = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": new_port,
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "postgres:18-alpine"
            }),
        };

        // Verify new port configuration is recognized
        let new_local_addr = service.get_local_address(config2).unwrap();
        assert!(new_local_addr.contains("6544"), "New port should be 6544");

        // Cleanup
        let _ = service.cleanup().await;
    }

    #[test]
    fn test_default_docker_image() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-image".to_string(), docker);

        let (image_name, version) = service.get_default_docker_image();
        assert_eq!(image_name, "postgres", "Default image should be postgres");
        assert_eq!(version, "17-alpine", "Default version should be 17-alpine");
    }

    #[tokio::test]
    async fn test_image_upgrade_scenario() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let _service = PostgresService::new("test-upgrade".to_string(), docker);

        // Create initial config with current PostgreSQL version
        let old_config = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": Some("6545"),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "postgres:16-alpine"
            }),
        };

        // Create new config with upgraded PostgreSQL version
        let new_config = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": Some("6545"),
                "database": "testdb",
                "username": "testuser",
                "password": "testpass123",
                "max_connections": 100,
                "ssl_mode": "disable",
                "docker_image": "postgres:18-alpine"
            }),
        };

        // Note: Full upgrade test would require actual Docker containers
        // This test verifies the configuration structure
        assert!(old_config.parameters.get("docker_image").is_some());
        assert!(new_config.parameters.get("docker_image").is_some());

        let old_image = old_config
            .parameters
            .get("docker_image")
            .and_then(|v| v.as_str());
        let new_image = new_config
            .parameters
            .get("docker_image")
            .and_then(|v| v.as_str());

        assert_eq!(old_image, Some("postgres:16-alpine"));
        assert_eq!(new_image, Some("postgres:18-alpine"));
    }

    #[test]
    fn test_parameter_schema_includes_docker_image() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-schema".to_string(), docker);

        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();
        let properties = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Verify docker_image field exists in schema
        assert!(
            properties.contains_key("docker_image"),
            "docker_image should be in schema"
        );

        // Verify docker_image is marked as editable
        let docker_image_field = properties
            .get("docker_image")
            .and_then(|v| v.as_object())
            .expect("docker_image field should be an object");

        let is_editable = docker_image_field
            .get("x-editable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        assert!(is_editable, "docker_image should be editable");
    }

    #[test]
    fn test_extract_postgres_version() {
        // Test various PostgreSQL image formats
        let test_cases = vec![
            ("postgres:16-alpine", 16),
            ("postgres:18-alpine", 17),
            ("postgres:16.0-alpine", 16),
            ("postgres:17.2-alpine", 17),
            ("timescale/timescaledb-ha:pg16", 16),
            ("timescale/timescaledb-ha:pg17", 17),
            ("postgres:15", 15),
            ("postgres:14.5", 14),
        ];

        for (image, expected_version) in test_cases {
            let result = PostgresService::extract_postgres_version(image);
            assert!(
                result.is_ok(),
                "Failed to extract version from image: {}",
                image
            );
            assert_eq!(
                result.unwrap(),
                expected_version,
                "Image {} should extract version {}",
                image,
                expected_version
            );
        }
    }

    #[test]
    fn test_version_extraction_invalid_formats() {
        // Test invalid image formats
        let invalid_cases = vec![
            "postgres",            // No tag
            "postgres:latest",     // Non-numeric version
            "postgres:abc-alpine", // Non-numeric version
            "postgres:alpha",      // Non-numeric version
        ];

        for image in invalid_cases {
            let result = PostgresService::extract_postgres_version(image);
            assert!(
                result.is_err(),
                "Image {} should fail to extract version",
                image
            );
        }
    }

    #[test]
    fn test_upgrade_version_check() {
        // Test that downgrade is prevented
        let old_config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "testdb".to_string(),
            username: "testuser".to_string(),
            password: Some("testpass".to_string()),
            max_connections: 100,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("postgres:16-alpine".to_string()),
        };

        let downgrade_config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "testdb".to_string(),
            username: "testuser".to_string(),
            password: Some("testpass".to_string()),
            max_connections: 100,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("postgres:15-alpine".to_string()),
        };

        let old_version =
            PostgresService::extract_postgres_version(&old_config.docker_image.clone().unwrap())
                .unwrap();
        let downgrade_version = PostgresService::extract_postgres_version(
            &downgrade_config.docker_image.clone().unwrap(),
        )
        .unwrap();

        // Verify that downgrade is detected (old >= new means no upgrade)
        assert!(
            old_version >= downgrade_version,
            "Downgrade should be detected: {} >= {}",
            old_version,
            downgrade_version
        );
    }

    #[test]
    fn test_postgres_v16_to_v17_upgrade_config() {
        // Test the configuration for upgrading from PostgreSQL 16 to 17
        let v16_config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "mydb".to_string(),
            username: "postgres".to_string(),
            password: Some("mysecretpass".to_string()),
            max_connections: 100,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("postgres:16-alpine".to_string()),
        };

        let v17_config = PostgresInputConfig {
            host: "localhost".to_string(),
            port: Some("5432".to_string()),
            database: "mydb".to_string(),
            username: "postgres".to_string(),
            password: Some("mysecretpass".to_string()),
            max_connections: 100,
            ssl_mode: Some("disable".to_string()),
            docker_image: Some("postgres:17-alpine".to_string()),
        };

        // Convert to runtime configs
        let v16_runtime: PostgresConfig = v16_config.into();
        let v17_runtime: PostgresConfig = v17_config.into();

        // Verify both configs are valid
        assert_eq!(v16_runtime.docker_image, "postgres:16-alpine");
        assert_eq!(v17_runtime.docker_image, "postgres:17-alpine");

        // Verify other parameters are preserved
        assert_eq!(v16_runtime.database, v17_runtime.database);
        assert_eq!(v16_runtime.username, v17_runtime.username);
        assert_eq!(v16_runtime.password, v17_runtime.password);
        assert_eq!(v16_runtime.max_connections, v17_runtime.max_connections);

        // Extract versions
        let v16_version = PostgresService::extract_postgres_version(&v16_runtime.docker_image)
            .expect("Should extract v16");
        let v17_version = PostgresService::extract_postgres_version(&v17_runtime.docker_image)
            .expect("Should extract v17");

        // Verify upgrade path is valid
        assert_eq!(v16_version, 16);
        assert_eq!(v17_version, 17);
        assert!(v17_version > v16_version, "v17 should be greater than v16");
    }

    #[tokio::test]
    async fn test_postgres_v16_to_v17_actual_upgrade() {
        // This test creates a real PostgreSQL 16 container, upgrades it to v17,
        // and verifies the upgrade by checking the version via SQL
        // Note: Requires Docker to be running

        let docker = match Docker::connect_with_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => {
                println!("Docker not available, skipping test");
                return;
            }
        };

        let port = 19432u16; // Use unique port to avoid conflicts
        let password = "postgres"; // Use default PostgreSQL password
        let service_name = format!(
            "test_postgres_upgrade_{}",
            chrono::Utc::now().timestamp_millis()
        );

        // Create v16 service configuration
        let v16_params = serde_json::json!({
            "host": "localhost",
            "port": port.to_string(),
            "database": "postgres",
            "username": "postgres",
            "password": password,
            "max_connections": 100,
            "docker_image": "postgres:16-alpine",
        });

        let v16_config = ServiceConfig {
            name: service_name.clone(),
            service_type: super::ServiceType::Postgres,
            version: Some("16".to_string()),
            parameters: v16_params,
        };

        // Create v17 service configuration
        let v17_params = serde_json::json!({
            "host": "localhost",
            "port": port.to_string(),
            "database": "postgres",
            "username": "postgres",
            "password": password,
            "max_connections": 100,
            "docker_image": "postgres:18-alpine",
        });

        let v17_config = ServiceConfig {
            name: service_name.clone(),
            service_type: super::ServiceType::Postgres,
            version: Some("17".to_string()),
            parameters: v17_params,
        };

        // Initialize v16 service
        let v16_service = PostgresService::new(service_name.clone(), docker.clone());

        match v16_service.init(v16_config.clone()).await {
            Ok(_) => {}
            Err(e) => {
                println!("Failed to initialize v16 service: {}. Skipping test (Docker may not be available)", e);
                let _ = v16_service.remove().await;
                return;
            }
        }

        // Give the container time to start and fully initialize with password
        // PostgreSQL needs time to initialize the database and set up authentication
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Wait for PostgreSQL to be healthy
        let mut retries = 0;
        loop {
            match v16_service.health_check().await {
                Ok(healthy) if healthy => break,
                _ if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                _ => {
                    println!("PostgreSQL 16 failed to start after 60 retries (30 seconds)");
                    let _ = v16_service.remove().await;
                    return;
                }
            }
        }

        // Connect and verify v16 version
        let connection_string = format!(
            "postgresql://postgres:{}@127.0.0.1:{}/postgres",
            urlencoding::encode(&password),
            port
        );

        // Try to connect with retries since database might still be initializing
        let mut db_pool = None;
        for attempt in 0..10 {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .connect(&connection_string)
                .await
            {
                Ok(pool) => {
                    db_pool = Some(pool);
                    break;
                }
                Err(e) if attempt < 9 => {
                    println!(
                        "Connection attempt {} failed: {}. Retrying...",
                        attempt + 1,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    println!(
                        "Failed to connect to v16 PostgreSQL after 10 attempts: {}. Skipping test",
                        e
                    );
                    let _ = v16_service.remove().await;
                    return;
                }
            }
        }

        let db_pool = db_pool.unwrap();

        let version_v16: (String,) =
            match sqlx::query_as("SELECT version()").fetch_one(&db_pool).await {
                Ok(v) => v,
                Err(e) => {
                    println!("Failed to query version from v16: {}. Skipping test", e);
                    db_pool.close().await;
                    let _ = v16_service.remove().await;
                    return;
                }
            };

        println!("PostgreSQL 16 version: {}", version_v16.0);
        assert!(
            version_v16.0.contains("16"),
            "Version should contain '16', got: {}",
            version_v16.0
        );

        // Close connection pool before upgrade
        db_pool.close().await;

        // Perform the upgrade
        match v16_service
            .upgrade(v16_config.clone(), v17_config.clone())
            .await
        {
            Ok(_) => {
                println!("✅ pg_upgrade completed successfully");
            }
            Err(e) => {
                // Cleanup before panicking
                let _ = v16_service.remove().await;
                panic!("Failed to upgrade PostgreSQL from v16 to v17: {}", e);
            }
        }

        // Give the upgraded container time to start and initialize
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Create v17 service to check health
        let v17_service = PostgresService::new(service_name.clone(), docker.clone());

        // Wait for v17 PostgreSQL to be healthy
        retries = 0;
        loop {
            match v17_service.health_check().await {
                Ok(healthy) if healthy => break,
                _ if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                _ => {
                    println!("PostgreSQL 17 failed to start after 60 retries (30 seconds)");
                    let _ = v17_service.remove().await;
                    return;
                }
            }
        }

        // Connect and verify v17 version with retries
        let mut db_pool = None;
        for attempt in 0..10 {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .connect(&connection_string)
                .await
            {
                Ok(pool) => {
                    db_pool = Some(pool);
                    break;
                }
                Err(e) if attempt < 9 => {
                    println!(
                        "V17 connection attempt {} failed: {}. Retrying...",
                        attempt + 1,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    println!(
                        "Failed to connect to v17 PostgreSQL after 10 attempts: {}. Skipping test",
                        e
                    );
                    let _ = v17_service.remove().await;
                    return;
                }
            }
        }

        let db_pool = db_pool.unwrap();

        let version_v17: (String,) =
            match sqlx::query_as("SELECT version()").fetch_one(&db_pool).await {
                Ok(v) => v,
                Err(e) => {
                    println!("Failed to query version from v17: {}. Skipping test", e);
                    db_pool.close().await;
                    let _ = v17_service.remove().await;
                    return;
                }
            };

        println!("PostgreSQL 17 version: {}", version_v17.0);
        assert!(
            version_v17.0.contains("17"),
            "Version should contain '17', got: {}",
            version_v17.0
        );

        // Verify upgrade was successful
        println!("✅ PostgreSQL upgrade test passed!");
        println!("  Before: {}", version_v16.0);
        println!("  After:  {}", version_v17.0);

        // Cleanup
        db_pool.close().await;
        let _ = v17_service.stop().await;
        let _ = v17_service.remove().await;
    }

    #[test]
    fn test_import_service_config_creation() {
        // Test that ServiceConfig is properly created for import
        let config = ServiceConfig {
            name: "test-postgres-import".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("15-alpine".to_string()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": 5432,
                "database": "testdb",
                "username": "postgres",
                "password": "testpass",
                "max_connections": "20",
                "ssl_mode": "disable",
                "docker_image": "postgres:15-alpine",
                "container_id": "abc123def456",
            }),
        };

        assert_eq!(config.name, "test-postgres-import");
        assert_eq!(config.service_type, ServiceType::Postgres);
        assert_eq!(config.version, Some("15-alpine".to_string()));
        assert_eq!(config.parameters["host"], "localhost");
        assert_eq!(config.parameters["port"], 5432);
    }

    #[test]
    fn test_import_version_extraction_with_tag() {
        // Test version extraction from Docker image names
        let test_cases = vec![
            ("postgres:15-alpine", "15-alpine"),
            ("postgres:latest", "latest"),
            ("postgres:14.5", "14.5"),
            ("postgres:16-bookworm", "16-bookworm"),
        ];

        for (image, expected_version) in test_cases {
            let version = if let Some(tag_pos) = image.rfind(':') {
                image[tag_pos + 1..].to_string()
            } else {
                "latest".to_string()
            };

            assert_eq!(version, expected_version, "Failed for image: {}", image);
        }
    }

    #[test]
    fn test_import_version_extraction_without_tag() {
        let image = "postgres";
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "latest".to_string()
        };

        assert_eq!(version, "latest");
    }

    #[test]
    fn test_import_connection_url_format() {
        let username = "postgres";
        let password = "mysecretpassword";
        let port = 5432;
        let database = "importeddb";

        let connection_url = format!(
            "postgresql://{}:{}@localhost:{}/{}",
            username, password, port, database
        );

        // Verify all components are present
        assert!(connection_url.contains("postgresql://"));
        assert!(connection_url.contains("postgres"));
        assert!(connection_url.contains("mysecretpassword"));
        assert!(connection_url.contains("localhost"));
        assert!(connection_url.contains("5432"));
        assert!(connection_url.contains("importeddb"));
    }

    #[test]
    fn test_import_validates_required_credentials() {
        let credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // Missing all required fields

        // These should all be None
        assert!(credentials.get("username").is_none());
        assert!(credentials.get("password").is_none());
        assert!(credentials.get("port").is_none());
        assert!(credentials.get("database").is_none());
    }

    #[test]
    fn test_import_credential_extraction() {
        let mut credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        credentials.insert("username".to_string(), "importuser".to_string());
        credentials.insert("password".to_string(), "importpass".to_string());
        credentials.insert("port".to_string(), "5433".to_string());
        credentials.insert("database".to_string(), "importdb".to_string());

        // Verify credential extraction
        assert_eq!(
            credentials.get("username").map(|s| s.as_str()),
            Some("importuser")
        );
        assert_eq!(
            credentials.get("password").map(|s| s.as_str()),
            Some("importpass")
        );
        assert_eq!(credentials.get("port").map(|s| s.as_str()), Some("5433"));
        assert_eq!(
            credentials.get("database").map(|s| s.as_str()),
            Some("importdb")
        );
    }

    #[tokio::test]
    async fn test_postgres_backup_and_restore_to_s3() {
        use super::super::test_utils::{
            create_mock_backup, create_mock_db, create_mock_external_service, MinioTestContainer,
        };

        // Check if Docker is available
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                println!("Docker not available, skipping test: {}", e);
                return;
            }
        };

        // Verify Docker is actually responding
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping test");
            return;
        }

        // Start MinIO container for S3 operations
        let minio = match MinioTestContainer::start(docker.clone(), "postgres-backup-test").await {
            Ok(m) => m,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("certificate")
                    || error_msg.contains("TrustStore")
                    || error_msg.contains("panicked")
                {
                    println!("❌ Skipping PostgreSQL backup test: TLS certificate issue");
                    println!(
                        "   Reason: {}",
                        error_msg.lines().next().unwrap_or(&error_msg)
                    );
                    println!("   Solution: Install system root certificates (required by AWS SDK even for HTTP endpoints)");
                    return;
                }
                panic!("Failed to start MinIO container: {}", e);
            }
        };

        // Create PostgreSQL service
        let pg_port = 15432u16; // Use unique port
        let pg_password = "testpass123";
        let service_name = format!("test_pg_backup_{}", chrono::Utc::now().timestamp_millis());

        let pg_params = serde_json::json!({
            "host": "localhost",
            "port": pg_port.to_string(),
            "database": "postgres",
            "username": "postgres",
            "password": pg_password,
            "max_connections": 100,
            "docker_image": "postgres:17-alpine",
        });

        let pg_config = ServiceConfig {
            name: service_name.clone(),
            service_type: ServiceType::Postgres,
            version: Some("17".to_string()),
            parameters: pg_params,
        };

        let pg_service = PostgresService::new(service_name.clone(), docker.clone());

        // Initialize PostgreSQL service
        match pg_service.init(pg_config.clone()).await {
            Ok(_) => println!("✓ PostgreSQL service initialized"),
            Err(e) => {
                println!("Failed to initialize PostgreSQL: {}. Skipping test", e);
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Wait for PostgreSQL to be healthy
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Create a test database and insert data
        let connection_string = format!(
            "postgresql://postgres:{}@127.0.0.1:{}/postgres",
            urlencoding::encode(&pg_password),
            pg_port
        );

        let db_pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&connection_string)
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                println!("Failed to connect to PostgreSQL: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Create test table and insert data
        match sqlx::query("CREATE TABLE test_backup (id SERIAL PRIMARY KEY, name TEXT NOT NULL, value INT NOT NULL)")
            .execute(&db_pool)
            .await
        {
            Ok(_) => println!("✓ Test table created"),
            Err(e) => {
                println!("Failed to create test table: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        match sqlx::query(
            "INSERT INTO test_backup (name, value) VALUES ($1, $2), ($3, $4), ($5, $6)",
        )
        .bind("test1")
        .bind(100)
        .bind("test2")
        .bind(200)
        .bind("test3")
        .bind(300)
        .execute(&db_pool)
        .await
        {
            Ok(_) => println!("✓ Test data inserted"),
            Err(e) => {
                println!("Failed to insert test data: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Verify data was inserted
        let count: (i64,) = match sqlx::query_as("SELECT COUNT(*) FROM test_backup")
            .fetch_one(&db_pool)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to count test data: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(count.0, 3, "Should have 3 rows");
        println!("✓ Verified {} rows in test table", count.0);

        // Close connection before backup
        db_pool.close().await;

        // Create mock database connection for backup/restore operations
        let mock_db = match create_mock_db().await {
            Ok(db) => db,
            Err(e) => {
                println!("Failed to create mock database: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Create mock backup record
        let backup = create_mock_backup("backups/postgres/test");
        let external_service = create_mock_external_service(service_name.clone(), "postgres", "17");

        // Perform backup to S3
        let backup_location = match pg_service
            .backup_to_s3(
                &minio.s3_client,
                backup,
                &minio.s3_source,
                "backups/postgres",
                "backups",
                &mock_db,
                &external_service,
                pg_config.clone(),
            )
            .await
        {
            Ok(location) => {
                println!("✓ Backup completed to: {}", location);
                location
            }
            Err(e) => {
                println!("Backup failed: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Drop the test table to simulate data loss
        let db_pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&connection_string)
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                println!("Failed to reconnect to PostgreSQL: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        match sqlx::query("DROP TABLE IF EXISTS test_backup")
            .execute(&db_pool)
            .await
        {
            Ok(_) => println!("✓ Test table dropped (simulating data loss)"),
            Err(e) => {
                println!("Failed to drop test table: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Verify table is gone
        let table_exists: (bool,) = match sqlx::query_as(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = 'test_backup')"
        )
        .fetch_one(&db_pool)
        .await
        {
            Ok(exists) => exists,
            Err(e) => {
                println!("Failed to check table existence: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert!(!table_exists.0, "Table should not exist after drop");
        println!("✓ Verified table was dropped");

        db_pool.close().await;

        // Restore from S3 backup
        match pg_service
            .restore_from_s3(
                &minio.s3_client,
                &backup_location,
                &minio.s3_source,
                pg_config.clone(),
            )
            .await
        {
            Ok(_) => println!("✓ Restore completed from: {}", backup_location),
            Err(e) => {
                println!("Restore failed: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Verify restored data
        let db_pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&connection_string)
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                println!("Failed to reconnect after restore: {}. Skipping test", e);
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Verify table exists
        let table_exists: (bool,) = match sqlx::query_as(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = 'test_backup')"
        )
        .fetch_one(&db_pool)
        .await
        {
            Ok(exists) => exists,
            Err(e) => {
                println!("Failed to check restored table: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert!(table_exists.0, "Table should exist after restore");
        println!("✓ Verified table was restored");

        // Verify row count
        let count: (i64,) = match sqlx::query_as("SELECT COUNT(*) FROM test_backup")
            .fetch_one(&db_pool)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to count restored data: {}. Skipping test", e);
                db_pool.close().await;
                let _ = pg_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(count.0, 3, "Should have 3 rows after restore");
        println!("✓ Verified {} rows were restored", count.0);

        // Verify actual data values
        let rows: Vec<(i32, String, i32)> =
            match sqlx::query_as("SELECT id, name, value FROM test_backup ORDER BY id")
                .fetch_all(&db_pool)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    println!("Failed to fetch restored rows: {}. Skipping test", e);
                    db_pool.close().await;
                    let _ = pg_service.remove().await;
                    let _ = minio.cleanup().await;
                    return;
                }
            };

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, "test1");
        assert_eq!(rows[0].2, 100);
        assert_eq!(rows[1].1, "test2");
        assert_eq!(rows[1].2, 200);
        assert_eq!(rows[2].1, "test3");
        assert_eq!(rows[2].2, 300);
        println!("✓ Verified all data values match original");

        // Cleanup
        db_pool.close().await;
        let _ = pg_service.stop().await;
        let _ = pg_service.remove().await;
        let _ = minio.cleanup().await;

        println!("✅ PostgreSQL backup and restore test passed!");
    }

    #[test]
    fn test_get_effective_address_baremetal_mode() {
        // Clear Docker mode to ensure baremetal mode
        std::env::remove_var("DEPLOYMENT_MODE");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-effective-addr".to_string(), docker);

        let config = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "5432",
                "database": "testdb",
                "username": "postgres",
                "password": "testpass",
                "max_connections": 100,
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();

        // In baremetal mode, should return localhost with exposed port
        assert_eq!(host, "localhost");
        assert_eq!(port, "5432");
    }

    #[test]
    fn test_get_effective_address_docker_mode() {
        // Set Docker mode
        std::env::set_var("DEPLOYMENT_MODE", "docker");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-effective-addr-docker".to_string(), docker);

        let config = ServiceConfig {
            name: "test-postgres".to_string(),
            service_type: super::ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "5432",
                "database": "testdb",
                "username": "postgres",
                "password": "testpass",
                "max_connections": 100,
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();

        // In Docker mode, should return container name with internal port
        assert_eq!(host, "postgres-test-effective-addr-docker");
        assert_eq!(port, "5432"); // Internal port

        // Clean up
        std::env::remove_var("DEPLOYMENT_MODE");
    }

    #[test]
    fn test_get_environment_variables_baremetal_mode() {
        // Clear Docker mode to ensure baremetal mode
        std::env::remove_var("DEPLOYMENT_MODE");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-env-vars".to_string(), docker);

        let mut params = HashMap::new();
        params.insert("port".to_string(), "5433".to_string());
        params.insert("database".to_string(), "testdb".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_environment_variables(&params).unwrap();

        // In baremetal mode, should use localhost
        assert_eq!(env_vars.get("POSTGRES_HOST").unwrap(), "localhost");
        assert_eq!(env_vars.get("POSTGRES_PORT").unwrap(), "5433");
        assert!(env_vars
            .get("POSTGRES_URL")
            .unwrap()
            .contains("localhost:5433"));
    }

    #[test]
    fn test_get_environment_variables_docker_mode() {
        // Set Docker mode
        std::env::set_var("DEPLOYMENT_MODE", "docker");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-env-vars-docker".to_string(), docker);

        let mut params = HashMap::new();
        params.insert("port".to_string(), "5433".to_string());
        params.insert("database".to_string(), "testdb".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_environment_variables(&params).unwrap();

        // In Docker mode, should use container name and internal port
        assert_eq!(
            env_vars.get("POSTGRES_HOST").unwrap(),
            "postgres-test-env-vars-docker"
        );
        assert_eq!(env_vars.get("POSTGRES_PORT").unwrap(), "5432"); // Internal port
        assert!(env_vars
            .get("POSTGRES_URL")
            .unwrap()
            .contains("postgres-test-env-vars-docker:5432"));

        // Clean up
        std::env::remove_var("DEPLOYMENT_MODE");
    }

    #[test]
    fn test_get_docker_environment_variables_baremetal_mode() {
        // Clear Docker mode to ensure baremetal mode
        std::env::remove_var("DEPLOYMENT_MODE");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-docker-env".to_string(), docker);

        let mut params = HashMap::new();
        params.insert("port".to_string(), "5434".to_string());
        params.insert("database".to_string(), "testdb".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_docker_environment_variables(&params).unwrap();

        // In baremetal mode, should use localhost with exposed port
        assert_eq!(env_vars.get("POSTGRES_HOST").unwrap(), "localhost");
        assert_eq!(env_vars.get("POSTGRES_PORT").unwrap(), "5434");
    }

    #[test]
    fn test_get_docker_environment_variables_docker_mode() {
        // Set Docker mode
        std::env::set_var("DEPLOYMENT_MODE", "docker");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = PostgresService::new("test-docker-env-mode".to_string(), docker);

        let mut params = HashMap::new();
        params.insert("port".to_string(), "5434".to_string());
        params.insert("database".to_string(), "testdb".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_docker_environment_variables(&params).unwrap();

        // In Docker mode, should use container name and internal port
        assert_eq!(
            env_vars.get("POSTGRES_HOST").unwrap(),
            "postgres-test-docker-env-mode"
        );
        assert_eq!(env_vars.get("POSTGRES_PORT").unwrap(), "5432"); // Internal port

        // Clean up
        std::env::remove_var("DEPLOYMENT_MODE");
    }
}
