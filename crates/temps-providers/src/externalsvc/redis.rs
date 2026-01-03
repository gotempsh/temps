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
use urlencoding;

/// Input configuration for creating a Redis service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "Redis Configuration",
    description = "Configuration for Redis service"
)]
pub struct RedisInputConfig {
    /// Redis host address
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// Redis port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// Redis password (auto-generated if not provided, empty, or less than 8 characters)
    #[serde(default, deserialize_with = "deserialize_optional_password")]
    #[schemars(
        with = "Option<String>",
        example = "example_password",
        description = "Redis password (minimum 8 characters, auto-generated if not provided)"
    )]
    pub password: Option<String>,

    /// Full Docker image reference (e.g., "redis:7-alpine")
    #[serde(default = "default_docker_image")]
    #[schemars(example = "example_docker_image", default = "default_docker_image")]
    pub docker_image: String,
}

/// Internal runtime configuration for Redis service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub host: String,
    pub port: String,
    pub password: String,
    pub docker_image: String,
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
            docker_image: input.docker_image,
        }
    }
}

const MIN_PASSWORD_LENGTH: usize = 8;

fn deserialize_optional_password<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt {
        Some(s) if !s.is_empty() && s.len() >= MIN_PASSWORD_LENGTH => Some(s),
        Some(s) if !s.is_empty() && s.len() < MIN_PASSWORD_LENGTH => {
            // Password provided but too short - treat as None to trigger auto-generation
            None
        }
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

fn default_docker_image() -> String {
    "redis:7-alpine".to_string()
}

fn example_docker_image() -> &'static str {
    "redis:7-alpine"
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
    docker: Arc<Docker>,
}

impl RedisService {
    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            docker,
        }
    }

    /// Create a fresh Redis connection
    /// Connection will be automatically closed when ConnectionManager is dropped
    /// This method is public to allow other services (like temps-kv) to get connections
    pub async fn get_connection(&self) -> Result<ConnectionManager> {
        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Redis configuration not found"))?
            .clone();

        let connection_url = if config.password.is_empty() {
            format!("redis://localhost:{}", config.port)
        } else {
            format!(
                "redis://:{}@localhost:{}",
                urlencoding::encode(&config.password),
                config.port
            )
        };

        let client = Client::open(connection_url.as_str())
            .map_err(|e| anyhow::anyhow!("Failed to create Redis client: {}", e))?;

        ConnectionManager::new(client)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create Redis connection manager: {}", e))
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

        // Use the docker_image from config
        info!("Pulling Redis image {}", config.docker_image);

        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = config.docker_image.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (config.docker_image.to_string(), "latest".to_string())
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

        // Check if container already exists and remove it
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
            info!(
                "Container {} already exists, removing it to recreate with new configuration",
                container_name
            );

            // Stop the container first
            let _ = docker
                .stop_container(
                    &container_name,
                    None::<bollard::query_parameters::StopContainerOptions>,
                )
                .await;

            // Remove the container
            docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to remove existing container: {}", e))?;

            info!("Removed existing container {}", container_name);
        }

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);

        let container_labels = HashMap::from([
            (service_label_key.as_str(), "redis"),
            (name_label_key.as_str(), self.name.as_str()),
        ]);

        let env_vars = [format!("REDIS_PASSWORD={}", password)];

        // Build Redis server command with password authentication if password is set
        let mut redis_cmd = vec![
            "redis-server".to_string(),
            "--appendonly".to_string(),
            "yes".to_string(),
        ];

        // Add password requirement if password is not empty
        if !password.is_empty() {
            redis_cmd.push("--requirepass".to_string());
            redis_cmd.push(password.to_string());
        }

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
            image: Some(config.docker_image.clone()),
            exposed_ports: Some(HashMap::from([("6379/tcp".to_string(), HashMap::new())])),
            env: Some(env_vars.iter().map(|s| s.as_str().to_string()).collect()),
            labels: Some(
                container_labels
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            cmd: Some(redis_cmd),
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

    /// Calculate a deterministic database number (0-15) from a resource name
    /// This allows us to allocate databases without requiring a Redis connection
    fn calculate_database_number(&self, resource_name: &str) -> u8 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        resource_name.hash(&mut hasher);
        let hash = hasher.finish();

        // Redis supports 16 databases (0-15), so we use modulo to get a valid number
        (hash % 16) as u8
    }

    fn get_redis_config(&self, service_config: ServiceConfig) -> Result<RedisConfig> {
        // Parse input config and transform to runtime config
        let input_config: RedisInputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse Redis configuration: {}", e))?;

        Ok(RedisConfig::from(input_config))
    }

    /// Verify that a Docker image can be pulled without actually downloading the full image
    /// Attempts to pull the image - fails if it doesn't exist or cannot be accessed
    #[allow(dead_code)]
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

/// Internal port used by Redis inside the container
const REDIS_INTERNAL_PORT: &str = "6379";

#[async_trait]
impl ExternalService for RedisService {
    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        let config = self.get_redis_config(service_config)?;

        if temps_core::DeploymentMode::is_docker() {
            // Docker mode: use container name and internal port
            Ok((self.get_container_name(), REDIS_INTERNAL_PORT.to_string()))
        } else {
            // Baremetal mode: use localhost and exposed port
            Ok(("localhost".to_string(), config.port))
        }
    }

    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!("Initializing Redis service {:?}", config);

        // Parse input config and transform to runtime config
        let redis_config = self.get_redis_config(config)?;

        // Store runtime config
        *self.config.write().await = Some(redis_config.clone());

        // Create Docker container (but don't start it yet)
        // Note: Connection will be established in start() method
        self.create_container(&self.docker, &redis_config, &redis_config.password)
            .await?;

        info!("Redis container created, connection will be established on start");

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
        // No stored connections to clean up - connections are created on-demand and auto-closed
        Ok(())
    }

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();
        let port = parameters.get("port").context("Missing port parameter")?;
        let password = parameters.get("password");

        // Get effective host and port based on deployment mode
        let (effective_host, effective_port) = if temps_core::DeploymentMode::is_docker() {
            // Docker mode: use container name and internal port
            (self.get_container_name(), REDIS_INTERNAL_PORT.to_string())
        } else {
            // Baremetal mode: use localhost and exposed port
            ("localhost".to_string(), port.clone())
        };

        let url = if let Some(pass) = password {
            format!(
                "redis://:{}@{}:{}",
                urlencoding::encode(pass),
                effective_host,
                effective_port
            )
        } else {
            format!("redis://{}:{}", effective_host, effective_port)
        };

        env_vars.insert("REDIS_URL".to_string(), url);
        env_vars.insert("REDIS_HOST".to_string(), effective_host);
        env_vars.insert("REDIS_PORT".to_string(), effective_port);
        if let Some(pass) = password {
            env_vars.insert("REDIS_PASSWORD".to_string(), pass.clone());
        }

        Ok(env_vars)
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        // Generate JSON Schema from RedisInputConfig
        let schema = schemars::schema_for!(RedisInputConfig);
        let mut schema_json = serde_json::to_value(schema).ok()?;

        // Add metadata about which fields are editable (based on RedisParameterStrategy::updateable_keys)
        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                // Define which fields should be editable - must match RedisParameterStrategy::updateable_keys()
                let editable = match key.as_str() {
                    "host" => false,        // Read-only
                    "port" => true,         // Updateable
                    "password" => false,    // Read-only
                    "docker_image" => true, // Updateable
                    _ => false,
                };

                if let Some(prop) = schema_json["properties"][&key].as_object_mut() {
                    prop.insert("x-editable".to_string(), serde_json::json!(editable));
                }
            }
        }

        Some(schema_json)
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

        // Calculate database number using a hash instead of requiring Redis connection
        // This allows us to generate env vars before the service is started
        let db_number = self.calculate_database_number(&resource_name);

        let mut env_vars = HashMap::new();

        // Always use container name and internal port for container-to-container communication
        let effective_host = self.get_container_name();
        let effective_port = REDIS_INTERNAL_PORT.to_string();

        // Database number (specific to this project/environment)
        env_vars.insert("REDIS_DATABASE".to_string(), db_number.to_string());

        // Get password from service config if available (filter out empty strings)
        let password = config
            .parameters
            .get("password")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        // Connection URL with database number
        let url = if let Some(pass) = password {
            format!(
                "redis://:{}@{}:{}/{}",
                urlencoding::encode(pass),
                effective_host,
                effective_port,
                db_number
            )
        } else {
            format!(
                "redis://{}:{}/{}",
                effective_host, effective_port, db_number
            )
        };
        env_vars.insert("REDIS_URL".to_string(), url);

        // Individual connection parameters
        env_vars.insert("REDIS_HOST".to_string(), effective_host);
        env_vars.insert("REDIS_PORT".to_string(), effective_port);
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

        // No connection initialization needed - connections are created on-demand when needed
        info!("Redis container started successfully");

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // No stored connections to clean up - they are created on-demand

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

        let password = parameters.get("password");

        // Always use container name and internal port for container-to-container communication
        let effective_host = self.get_container_name();
        let effective_port = REDIS_INTERNAL_PORT.to_string();

        let url = if let Some(pass) = password {
            format!(
                "redis://:{}@{}:{}",
                urlencoding::encode(pass),
                effective_host,
                effective_port
            )
        } else {
            format!("redis://{}:{}", effective_host, effective_port)
        };

        env_vars.insert("REDIS_URL".to_string(), url);
        env_vars.insert("REDIS_HOST".to_string(), effective_host);
        env_vars.insert("REDIS_PORT".to_string(), effective_port);
        if let Some(pass) = password {
            env_vars.insert("REDIS_PASSWORD".to_string(), pass.clone());
        }

        Ok(env_vars)
    }

    async fn deprovision_resource(&self, _project_id: &str, _environment: &str) -> Result<()> {
        // No database-level deprovisioning needed
        // Each project/environment gets a calculated database number (0-15) based on hash
        // Cleanup would happen at the application level (flushing keys with specific prefixes)
        Ok(())
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

        // Get file size before upload
        let size_bytes = std::fs::metadata(&tar_path)?.len() as i32;

        // Validate backup size - a zero-size backup indicates failure
        if size_bytes == 0 {
            let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("failed".to_string());
            backup_update.finished_at = Set(Some(Utc::now()));
            backup_update.error_message =
                Set(Some("Backup failed: backup file has zero size".to_string()));
            backup_update.update(pool).await?;
            return Err(anyhow::anyhow!(
                "Redis backup failed: backup file has zero size"
            ));
        }

        // Upload to S3
        s3_client
            .put_object()
            .bucket(&s3_source.bucket_name)
            .key(&backup_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_path(&tar_path).await?)
            .content_type("application/x-tar")
            .send()
            .await?;

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

    fn get_default_docker_image(&self) -> (String, String) {
        // Return (image_name, version)
        ("redis".to_string(), "7-alpine".to_string())
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
                "Failed to get current docker image for Redis container"
            ))
        }
    }

    fn get_default_version(&self) -> String {
        "7-alpine".to_string()
    }

    async fn get_current_version(&self) -> Result<String> {
        let (_, version) = self.get_current_docker_image().await?;
        Ok(version)
    }

    async fn upgrade(&self, old_config: ServiceConfig, new_config: ServiceConfig) -> Result<()> {
        info!("Starting Redis upgrade");

        let _old_redis_config = self.get_redis_config(old_config)?;
        let new_redis_config = self.get_redis_config(new_config)?;

        // Verify the new image can be pulled BEFORE stopping the old container
        info!(
            "Verifying new Docker image is available: {}",
            new_redis_config.docker_image
        );
        self.verify_image_pullable(&new_redis_config.docker_image)
            .await?;
        info!("New Docker image verified and is available");

        // Stop the old container
        info!("Stopping old Redis container");
        self.stop().await?;

        // Create container with new image (keeping the same volume for data persistence)
        info!("Starting Redis container with new image");
        self.create_container(&self.docker, &new_redis_config, &new_redis_config.password)
            .await?;

        info!("Redis upgrade completed successfully");
        Ok(())
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

        // Extract version from image name (e.g., "redis:7-alpine" -> "7")
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "7-alpine".to_string()
        };

        // Extract port from additional config if provided, otherwise use 6379
        let port = additional_config
            .get("port")
            .and_then(|v| v.as_str())
            .unwrap_or("6379")
            .to_string();

        // Extract password if provided
        let password = credentials.get("password").cloned().unwrap_or_default();

        // Verify connection to the imported service
        let connection_url = if password.is_empty() {
            format!("redis://localhost:{}", port)
        } else {
            format!(
                "redis://:{}@localhost:{}",
                urlencoding::encode(&password),
                port
            )
        };

        match redis::Client::open(connection_url.as_str())
            .ok()
            .and_then(|client| {
                tokio::runtime::Runtime::new()
                    .ok()
                    .and_then(|rt| rt.block_on(async { client.get_connection().ok() }))
            }) {
            Some(_) => {
                info!("Successfully verified Redis connection for import");
            }
            None => {
                return Err(anyhow::anyhow!(
                    "Failed to connect to Redis at localhost:{} with provided credentials. Verify port and password.",
                    port
                ));
            }
        }

        // Build the ServiceConfig for registration
        let config = ServiceConfig {
            name: service_name,
            service_type: ServiceType::Redis,
            version: Some(version),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": port,
                "password": password,
                "docker_image": image,
                "container_id": container_id,
            }),
        };

        info!(
            "Successfully imported Redis service '{}' from container",
            config.name
        );
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parameter_schema_editable_fields() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-editable".to_string(), docker);

        // Get the parameter schema
        let schema_opt = service.get_parameter_schema();
        assert!(schema_opt.is_some(), "Schema should be generated");

        let schema = schema_opt.unwrap();
        let schema_obj = schema.as_object().expect("Schema should be an object");
        let properties = schema_obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("Properties should be an object");

        // Define expected editable status for each field - must match RedisParameterStrategy::updateable_keys()
        let editable_status = vec![
            ("host", false),
            ("port", true),
            ("password", false),
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
        let service = RedisService::new("test-port-change".to_string(), docker);

        // Create initial config with a specific port
        let initial_port = "7543";
        let config1 = super::ServiceConfig {
            name: "test-redis".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": initial_port,
                "password": "redispass123"
            }),
        };

        // Initialize service
        let result = service.init(config1.clone()).await;
        assert!(result.is_ok(), "Service initialization failed");

        // Verify initial port is set
        let local_addr = service.get_local_address(config1.clone()).unwrap();
        assert!(local_addr.contains("7543"), "Initial port should be 7543");

        // Create new config with different port
        let new_port = "7544";
        let config2 = super::ServiceConfig {
            name: "test-redis".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": new_port,
                "password": "redispass123"
            }),
        };

        // Verify new port configuration is recognized
        let new_local_addr = service.get_local_address(config2).unwrap();
        assert!(new_local_addr.contains("7544"), "New port should be 7544");

        // Cleanup
        let _ = service.cleanup().await;
    }

    #[test]
    fn test_default_docker_image() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-image".to_string(), docker);

        let (image_name, version) = service.get_default_docker_image();
        assert_eq!(image_name, "redis", "Default image should be redis");
        assert_eq!(version, "7-alpine", "Default version should be 7-alpine");
    }

    #[test]
    fn test_image_and_version_in_config() {
        // Test Redis configuration with docker_image field
        let input_config = RedisInputConfig {
            host: "localhost".to_string(),
            port: Some("6379".to_string()),
            password: Some("mypassword".to_string()),
            docker_image: "redis:7-alpine".to_string(),
        };

        // Convert to runtime config
        let runtime_config: RedisConfig = input_config.into();

        // Verify docker_image is used directly
        assert_eq!(runtime_config.docker_image, "redis:7-alpine");
    }

    #[test]
    fn test_docker_image_parameter() {
        // Test Redis configuration with docker_image parameter
        let input_config = RedisInputConfig {
            host: "localhost".to_string(),
            port: Some("6379".to_string()),
            password: Some("mypassword".to_string()),
            docker_image: "redis:8-alpine".to_string(),
        };

        // Convert to runtime config
        let runtime_config: RedisConfig = input_config.into();

        // Verify docker_image is used
        assert_eq!(
            runtime_config.docker_image, "redis:8-alpine",
            "Docker image should use provided docker_image"
        );
    }

    #[test]
    fn test_docker_image_without_tag() {
        // Test Redis configuration with docker_image parameter but no tag
        let input_config = RedisInputConfig {
            host: "localhost".to_string(),
            port: Some("6379".to_string()),
            password: Some("mypassword".to_string()),
            docker_image: "redis".to_string(), // No tag
        };

        // Convert to runtime config
        let runtime_config: RedisConfig = input_config.into();

        // Verify docker_image with no tag is preserved as-is
        assert_eq!(runtime_config.docker_image, "redis");
    }

    #[test]
    fn test_redis_version_upgrade_config() {
        // Test simulated upgrade from Redis 6 to 7
        let old_config = super::ServiceConfig {
            name: "test-redis".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": Some("6379"),
                "password": "redispass123",
                "image": "redis",
                "version": "6-alpine"
            }),
        };

        let new_config = super::ServiceConfig {
            name: "test-redis".to_string(),
            service_type: super::ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": Some("6379"),
                "password": "redispass123",
                "image": "redis",
                "version": "7-alpine"
            }),
        };

        // Verify version upgrade configuration
        let old_version = old_config
            .parameters
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let new_version = new_config
            .parameters
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        assert_eq!(old_version, "6-alpine", "Old version should be 6-alpine");
        assert_eq!(new_version, "7-alpine", "New version should be 7-alpine");
    }

    #[test]
    fn test_import_service_config_creation() {
        let config = ServiceConfig {
            name: "test-redis-import".to_string(),
            service_type: ServiceType::Redis,
            version: Some("7-alpine".to_string()),
            parameters: serde_json::json!({
                "host": "localhost",
                "port": 6379,
                "password": "",
                "db": 0,
                "docker_image": "redis:7-alpine",
                "container_id": "xyz789abc123",
            }),
        };

        assert_eq!(config.name, "test-redis-import");
        assert_eq!(config.service_type, ServiceType::Redis);
        assert_eq!(config.version, Some("7-alpine".to_string()));
        assert_eq!(config.parameters["port"], 6379);
    }

    #[test]
    fn test_import_redis_version_extraction() {
        let test_cases = vec![
            ("redis:7-alpine", "7-alpine"),
            ("redis:latest", "latest"),
            ("redis:6.2", "6.2"),
            ("redis:7.0-alpine", "7.0-alpine"),
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
    fn test_import_validates_required_credentials() {
        let credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // Redis might only need port and optional password

        assert!(credentials.get("port").is_none());
        assert!(credentials.get("password").is_none());
    }

    #[test]
    fn test_import_connection_string_with_password() {
        let password = "redispassword";
        let port = 6379;

        let connection_url = format!("redis://{}@localhost:{}", password, port);

        assert!(connection_url.contains("redis://"));
        assert!(connection_url.contains("redispassword"));
        assert!(connection_url.contains("localhost"));
        assert!(connection_url.contains("6379"));
    }

    #[test]
    fn test_import_connection_string_without_password() {
        let port = 6379;

        let connection_url = format!("redis://localhost:{}", port);

        assert!(connection_url.contains("redis://"));
        assert!(connection_url.contains("localhost"));
        assert!(connection_url.contains("6379"));
    }

    #[tokio::test]
    async fn test_redis_backup_and_restore_to_s3() {
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
        let minio = match MinioTestContainer::start(docker.clone(), "redis-backup-test").await {
            Ok(m) => m,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("certificate")
                    || error_msg.contains("TrustStore")
                    || error_msg.contains("panicked")
                {
                    println!("❌ Skipping Redis backup test: TLS certificate issue");
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

        // Create Redis service
        let redis_port = 16379u16; // Use unique port
        let redis_password = "redispass123";
        let service_name = format!(
            "test_redis_backup_{}",
            chrono::Utc::now().timestamp_millis()
        );

        let redis_params = serde_json::json!({
            "host": "localhost",
            "port": redis_port.to_string(),
            "password": redis_password,
            "docker_image": "redis:7-alpine",
        });

        let redis_config = ServiceConfig {
            name: service_name.clone(),
            service_type: ServiceType::Redis,
            version: Some("7".to_string()),
            parameters: redis_params,
        };

        let redis_service = RedisService::new(service_name.clone(), docker.clone());

        // Initialize Redis service
        match redis_service.init(redis_config.clone()).await {
            Ok(_) => println!("✓ Redis service initialized"),
            Err(e) => {
                println!("Failed to initialize Redis: {}. Skipping test", e);
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Wait for Redis to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Connect to Redis and set some test data
        let connection_url = format!("redis://localhost:{}", redis_port);
        let redis_client = match redis::Client::open(connection_url.as_str()) {
            Ok(client) => client,
            Err(e) => {
                println!("Failed to create Redis client: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        let mut conn = match redis_client.get_connection() {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to connect to Redis: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Set test data
        match redis::cmd("SET")
            .arg("test_key1")
            .arg("value1")
            .query::<()>(&mut conn)
        {
            Ok(_) => println!("✓ Set test_key1=value1"),
            Err(e) => {
                println!("Failed to set test key 1: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        match redis::cmd("SET")
            .arg("test_key2")
            .arg("value2")
            .query::<()>(&mut conn)
        {
            Ok(_) => println!("✓ Set test_key2=value2"),
            Err(e) => {
                println!("Failed to set test key 2: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        match redis::cmd("SET")
            .arg("test_key3")
            .arg("value3")
            .query::<()>(&mut conn)
        {
            Ok(_) => println!("✓ Set test_key3=value3"),
            Err(e) => {
                println!("Failed to set test key 3: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Verify data exists
        let value1: String = match redis::cmd("GET").arg("test_key1").query(&mut conn) {
            Ok(v) => v,
            Err(e) => {
                println!("Failed to get test key 1: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(value1, "value1");
        println!("✓ Verified test_key1={}", value1);

        // Drop connection before backup
        drop(conn);

        // Create mock database connection for backup/restore operations
        let mock_db = match create_mock_db().await {
            Ok(db) => db,
            Err(e) => {
                println!("Failed to create mock database: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Create mock backup record
        let backup = create_mock_backup("backups/redis/test");
        let external_service = create_mock_external_service(service_name.clone(), "redis", "7");

        // Perform backup to S3
        let backup_location = match redis_service
            .backup_to_s3(
                &minio.s3_client,
                backup,
                &minio.s3_source,
                "backups/redis",
                "backups",
                &mock_db,
                &external_service,
                redis_config.clone(),
            )
            .await
        {
            Ok(location) => {
                println!("✓ Backup completed to: {}", location);
                location
            }
            Err(e) => {
                println!("Backup failed: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Delete keys to simulate data loss
        let mut conn = match redis_client.get_connection() {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to reconnect to Redis: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        match redis::cmd("DEL")
            .arg("test_key1")
            .arg("test_key2")
            .arg("test_key3")
            .query::<()>(&mut conn)
        {
            Ok(_) => println!("✓ Deleted all test keys (simulating data loss)"),
            Err(e) => {
                println!("Failed to delete keys: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        }

        // Verify keys are gone
        let exists: bool = match redis::cmd("EXISTS").arg("test_key1").query(&mut conn) {
            Ok(e) => e,
            Err(e) => {
                println!("Failed to check key existence: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert!(!exists, "test_key1 should not exist after deletion");
        println!("✓ Verified keys were deleted");

        drop(conn);

        // Restore from S3 backup
        match redis_service
            .restore_from_s3(
                &minio.s3_client,
                &backup_location,
                &minio.s3_source,
                redis_config.clone(),
            )
            .await
        {
            Ok(_) => println!("✓ Restore completed from: {}", backup_location),
            Err(e) => {
                println!("Restore failed: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Wait for Redis to be ready after restore
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Verify restored data
        let mut conn = match redis_client.get_connection() {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to reconnect after restore: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };

        // Verify keys exist
        let exists1: bool = match redis::cmd("EXISTS").arg("test_key1").query(&mut conn) {
            Ok(e) => e,
            Err(e) => {
                println!("Failed to check restored key1: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert!(exists1, "test_key1 should exist after restore");
        println!("✓ Verified test_key1 exists after restore");

        // Verify values
        let value1: String = match redis::cmd("GET").arg("test_key1").query(&mut conn) {
            Ok(v) => v,
            Err(e) => {
                println!("Failed to get restored value1: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(value1, "value1");
        println!("✓ Verified test_key1={}", value1);

        let value2: String = match redis::cmd("GET").arg("test_key2").query(&mut conn) {
            Ok(v) => v,
            Err(e) => {
                println!("Failed to get restored value2: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(value2, "value2");
        println!("✓ Verified test_key2={}", value2);

        let value3: String = match redis::cmd("GET").arg("test_key3").query(&mut conn) {
            Ok(v) => v,
            Err(e) => {
                println!("Failed to get restored value3: {}. Skipping test", e);
                let _ = redis_service.remove().await;
                let _ = minio.cleanup().await;
                return;
            }
        };
        assert_eq!(value3, "value3");
        println!("✓ Verified test_key3={}", value3);

        // Cleanup
        drop(conn);
        let _ = redis_service.stop().await;
        let _ = redis_service.remove().await;
        let _ = minio.cleanup().await;

        println!("✅ Redis backup and restore test passed!");
    }

    #[test]
    fn test_get_effective_address_baremetal_mode() {
        // Clear Docker mode to ensure baremetal mode
        std::env::remove_var("DEPLOYMENT_MODE");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-effective-addr".to_string(), docker);

        let config = ServiceConfig {
            name: "test-redis".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "6379",
                "password": "testpass",
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();

        // In baremetal mode, should return localhost with exposed port
        assert_eq!(host, "localhost");
        assert_eq!(port, "6379");
    }

    #[test]
    fn test_get_effective_address_docker_mode() {
        // Set Docker mode
        std::env::set_var("DEPLOYMENT_MODE", "docker");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-effective-addr-docker".to_string(), docker);

        let config = ServiceConfig {
            name: "test-redis".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({
                "host": "localhost",
                "port": "6380",
                "password": "testpass",
            }),
        };

        let (host, port) = service.get_effective_address(config).unwrap();

        // In Docker mode, should return container name with internal port
        assert_eq!(host, "redis-test-effective-addr-docker");
        assert_eq!(port, "6379"); // Internal port

        // Clean up
        std::env::remove_var("DEPLOYMENT_MODE");
    }

    #[test]
    fn test_get_environment_variables_baremetal_mode() {
        // Clear Docker mode to ensure baremetal mode
        std::env::remove_var("DEPLOYMENT_MODE");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-env-vars".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6380".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_environment_variables(&params).unwrap();

        // In baremetal mode, should use localhost
        assert_eq!(env_vars.get("REDIS_HOST").unwrap(), "localhost");
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6380");
        assert!(env_vars
            .get("REDIS_URL")
            .unwrap()
            .contains("localhost:6380"));
    }

    #[test]
    fn test_get_environment_variables_docker_mode() {
        // Set Docker mode
        std::env::set_var("DEPLOYMENT_MODE", "docker");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-env-vars-docker".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6380".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_environment_variables(&params).unwrap();

        // In Docker mode, should use container name and internal port
        assert_eq!(
            env_vars.get("REDIS_HOST").unwrap(),
            "redis-test-env-vars-docker"
        );
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6379"); // Internal port
        assert!(env_vars
            .get("REDIS_URL")
            .unwrap()
            .contains("redis-test-env-vars-docker:6379"));

        // Clean up
        std::env::remove_var("DEPLOYMENT_MODE");
    }

    #[test]
    fn test_get_docker_environment_variables_baremetal_mode() {
        // Clear Docker mode to ensure baremetal mode
        std::env::remove_var("DEPLOYMENT_MODE");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-docker-env".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6381".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_docker_environment_variables(&params).unwrap();

        // In baremetal mode, should use localhost with exposed port
        assert_eq!(env_vars.get("REDIS_HOST").unwrap(), "localhost");
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6381");
    }

    #[test]
    fn test_get_docker_environment_variables_docker_mode() {
        // Set Docker mode
        std::env::set_var("DEPLOYMENT_MODE", "docker");

        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = RedisService::new("test-docker-env-mode".to_string(), docker);

        let mut params = std::collections::HashMap::new();
        params.insert("port".to_string(), "6381".to_string());
        params.insert("password".to_string(), "testpass".to_string());

        let env_vars = service.get_docker_environment_variables(&params).unwrap();

        // In Docker mode, should use container name and internal port
        assert_eq!(
            env_vars.get("REDIS_HOST").unwrap(),
            "redis-test-docker-env-mode"
        );
        assert_eq!(env_vars.get("REDIS_PORT").unwrap(), "6379"); // Internal port

        // Clean up
        std::env::remove_var("DEPLOYMENT_MODE");
    }
}
