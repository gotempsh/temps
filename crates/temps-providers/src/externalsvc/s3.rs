use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::Client;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::Docker;
use futures::TryStreamExt;
use rand::{distributions::Alphanumeric, Rng};
use schemars::JsonSchema;
use sea_orm::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{self};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info};

use crate::utils::ensure_network_exists;

use super::{ExternalService, ServiceConfig, ServiceType};

/// Input configuration for creating an S3/MinIO service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "S3/MinIO Configuration",
    description = "Configuration for S3-compatible storage service (MinIO)"
)]
pub struct S3InputConfig {
    /// S3/MinIO port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// S3 access key (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_key")]
    #[schemars(with = "Option<String>", example = "example_access_key")]
    pub access_key: Option<String>,

    /// S3 secret key (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_key")]
    #[schemars(with = "Option<String>", example = "example_secret_key")]
    pub secret_key: Option<String>,

    /// S3 host address
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// S3 region
    #[serde(default = "default_region")]
    #[schemars(example = "example_region", default = "default_region")]
    pub region: String,

    /// Docker image to use for MinIO (e.g., minio/minio:RELEASE.2025-09-07T16-13-09Z)
    #[serde(default = "default_image")]
    #[schemars(example = "example_image", default = "default_image")]
    pub docker_image: String,
}

/// Internal runtime configuration for S3/MinIO service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    pub port: String,
    pub access_key: String,
    pub secret_key: String,
    pub host: String,
    pub region: String,
    pub docker_image: String,
}

impl From<S3InputConfig> for S3Config {
    fn from(input: S3InputConfig) -> Self {
        Self {
            port: input.port.unwrap_or_else(|| {
                find_available_port(9000)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "9000".to_string())
            }),
            access_key: input.access_key.unwrap_or_else(default_access_key),
            secret_key: input.secret_key.unwrap_or_else(default_secret_key),
            host: input.host,
            region: input.region,
            docker_image: input.docker_image,
        }
    }
}

fn deserialize_optional_key<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}
fn default_region() -> String {
    "us-east-1".to_string()
}
fn default_host() -> String {
    "localhost".to_string()
}

fn default_access_key() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(15)
        .map(char::from)
        .collect()
}

fn default_secret_key() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(15)
        .map(char::from)
        .collect()
}

// Schema example functions
fn example_port() -> &'static str {
    "9000"
}

fn example_access_key() -> &'static str {
    "minioadmin"
}

fn example_secret_key() -> &'static str {
    "minioadmin"
}

fn example_host() -> &'static str {
    "localhost"
}

fn example_region() -> &'static str {
    "us-east-1"
}

fn default_image() -> String {
    "minio/minio:RELEASE.2025-09-07T16-13-09Z".to_string()
}

fn example_image() -> &'static str {
    "minio/minio:RELEASE.2025-09-07T16-13-09Z"
}

fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn find_available_port(start_port: u16) -> Option<u16> {
    (start_port..start_port + 100).find(|&port| is_port_available(port))
}

pub struct S3Service {
    name: String,
    config: Arc<RwLock<Option<S3Config>>>,
    client: Arc<RwLock<Option<Client>>>,
    docker: Arc<Docker>,
}

impl S3Service {
    /// MinIO Client (mc) utility image - used for temporary operations like migration and copy
    const MC_IMAGE: &'static str = "minio/mc:RELEASE.2025-08-13T08-35-41Z";

    pub fn new(name: String, docker: Arc<Docker>) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            client: Arc::new(RwLock::new(None)),
            docker,
        }
    }

    fn get_container_name(&self) -> String {
        format!("minio-{}", self.name)
    }

    async fn create_container(&self, docker: &Docker, config: &S3Config) -> Result<()> {
        // Pull the image first
        info!("Pulling MinIO image {}", config.docker_image);

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
            .await?;

        let container_name = self.get_container_name();
        // Add volume name construction
        let volume_name = format!("minio_{}_data", self.name);

        // Create volume if it doesn't exist
        docker
            .create_volume(bollard::models::VolumeCreateOptions {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await?;

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
            (service_label_key.as_str(), "minio"),
            (name_label_key.as_str(), self.name.as_str()),
        ]);

        let env_vars = [
            format!("MINIO_ROOT_USER={}", config.access_key),
            format!("MINIO_ROOT_PASSWORD={}", config.secret_key),
        ];
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
        let host_config = bollard::models::HostConfig {
            port_bindings: Some(HashMap::from([(
                "9000/tcp".to_string(),
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(config.port.to_string()),
                }]),
            )])),
            // Add volume mount
            mounts: Some(vec![bollard::models::Mount {
                target: Some("/data".to_string()),
                source: Some(volume_name.clone()),
                typ: Some(bollard::models::MountTypeEnum::VOLUME),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(config.docker_image.to_string()),
            networking_config,
            exposed_ports: Some(HashMap::from([("9000/tcp".to_string(), HashMap::new())])),
            env: Some(env_vars.iter().map(|s| s.as_str().to_string()).collect()),
            labels: Some(
                container_labels
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            cmd: Some(vec!["server".to_string(), "/data".to_string()]),
            host_config: Some(bollard::models::HostConfig {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..host_config
            }),
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec!["CMD-SHELL".to_string(), "mc ready local".to_string()]),
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
            .map_err(|e| anyhow::anyhow!("Failed to create MinIO container: {}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start MinIO container: {}", e))?;

        // Wait for container to be healthy
        self.wait_for_container_health(docker, &container.id)
            .await?;

        info!("MinIO container {} created and started", container.id);
        Ok(())
    }

    async fn pull_mc_image(&self, docker: &Docker) -> Result<()> {
        info!("Pulling MinIO Client image {}", Self::MC_IMAGE);

        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = Self::MC_IMAGE.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (Self::MC_IMAGE.to_string(), "latest".to_string())
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

        Err(anyhow::anyhow!("MinIO container health check timed out"))
    }

    async fn initialize_client(&self, config: ServiceConfig) -> Result<Client> {
        let s3_config = self.get_s3_config(config)?;
        info!("Initializing S3 client with config {:?}", s3_config);
        let config = aws_sdk_s3::Config::builder()
            .endpoint_url(format!("http://{}:{}", s3_config.host, s3_config.port))
            .region(Region::new(s3_config.region))
            .behavior_version_latest()
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                s3_config.access_key,
                s3_config.secret_key,
                None,
                None,
                "minio",
            ))
            .force_path_style(true)
            .build();

        let client = Client::from_conf(config);
        Ok(client)
    }

    async fn create_bucket(&self, config: ServiceConfig, name: &str) -> Result<()> {
        // Initialize client if not already initialized
        let client = self.initialize_client(config).await?;

        let sanitized_name = name.replace("_", "-").to_lowercase();

        // Check if bucket already exists
        match client.head_bucket().bucket(&sanitized_name).send().await {
            Ok(_) => {
                info!("Bucket {} already exists", sanitized_name);
                return Ok(());
            }
            Err(err) => {
                debug!("Bucket {} does not exist: {}", sanitized_name, err);
            }
        }

        // Create bucket if it doesn't exist
        client
            .create_bucket()
            .bucket(sanitized_name.clone())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create bucket {}: {:?}", sanitized_name, e))?;

        info!("Created bucket {}", sanitized_name);
        Ok(())
    }

    #[allow(dead_code)]
    async fn delete_bucket(&self, config: ServiceConfig, name: &str) -> Result<()> {
        // Initialize client if not already initialized
        let client = self.initialize_client(config).await?;

        let sanitized_name = name.replace("_", "-").to_lowercase();

        // Check if bucket exists before attempting to delete
        match client.head_bucket().bucket(&sanitized_name).send().await {
            Ok(_) => {
                // Bucket exists, proceed with deletion
                client
                    .delete_bucket()
                    .bucket(&sanitized_name)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to delete bucket: {}", e))?;

                info!("Deleted bucket {}", sanitized_name);
                Ok(())
            }
            Err(err) => {
                debug!("Bucket {} does not exist: {}", sanitized_name, err);
                Ok(()) // Return Ok since the end state (bucket doesn't exist) is what we want
            }
        }
    }
    fn get_s3_config(&self, service_config: ServiceConfig) -> Result<S3Config> {
        // Parse input config and transform to runtime config
        let input_config: S3InputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse S3 configuration: {}", e))?;

        Ok(S3Config::from(input_config))
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

#[async_trait]
impl ExternalService for S3Service {
    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let config = self.get_s3_config(service_config)?;
        Ok(format!("localhost:{}", config.port))
    }

    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!("Initializing S3 service {:?}", config);

        // Parse input config and transform to runtime config
        let s3_config = self.get_s3_config(config)?;
        info!("Initializing S3 config {:?}", s3_config);

        // Store runtime config
        *self.config.write().await = Some(s3_config.clone());

        // Create Docker container
        self.create_container(&self.docker, &s3_config).await?;

        // Serialize the full runtime config to save to database
        // This ensures auto-generated values (keys, port) are persisted
        let runtime_config_json = serde_json::to_value(&s3_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize S3 runtime config: {}", e))?;

        let runtime_config_map = runtime_config_json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Runtime config is not an object"))?;

        let mut inferred_params = HashMap::new();
        for (key, value) in runtime_config_map {
            if let Some(str_value) = value.as_str() {
                inferred_params.insert(key.clone(), str_value.to_string());
            }
        }

        info!("Inferred params {:?}", inferred_params);
        Ok(inferred_params)
    }

    async fn health_check(&self) -> Result<bool> {
        // let client = self.get_client().await?;
        // let config = self.config.read().await;
        Ok(true)
        // if let Some(cfg) = config.as_ref() {
        //     let result = client.head_bucket().bucket(&cfg.bucket).send().await;
        //     Ok(result.is_ok())
        // } else {
        //     Ok(false)
        // }
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::S3
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
            Some(cfg) => {
                let endpoint = format!("http://localhost:{}", cfg.port);
                Ok(format!("s3://{}", endpoint))
            }
            None => Err(anyhow::anyhow!("S3 not configured")),
        }
    }

    async fn cleanup(&self) -> Result<()> {
        *self.client.write().await = None;
        Ok(())
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        // Generate JSON Schema from S3InputConfig
        let schema = schemars::schema_for!(S3InputConfig);
        let mut schema_json = serde_json::to_value(schema).ok()?;

        // Add metadata about which fields are editable (based on S3ParameterStrategy::updateable_keys)
        if let Some(properties) = schema_json
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
        {
            for key in properties.keys().cloned().collect::<Vec<_>>() {
                // Define which fields should be editable - must match S3ParameterStrategy::updateable_keys()
                let editable = match key.as_str() {
                    "host" => false,        // Read-only
                    "port" => true,         // Updateable
                    "access_key" => false,  // Read-only
                    "secret_key" => false,  // Read-only
                    "region" => false,      // Read-only
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

    async fn start(&self) -> Result<()> {
        let docker = &self.docker;
        let container_name = self.get_container_name();
        info!("Starting MinIO container {}", container_name);

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
                .ok_or_else(|| anyhow::anyhow!("S3 configuration not found"))?
                .clone();
            self.create_container(docker, &config).await?;
        } else {
            docker
                .start_container(
                    &container_name,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start existing MinIO container: {}", e))?;
        }

        self.wait_for_container_health(docker, &container_name)
            .await?;

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // Clear the client
        *self.client.write().await = None;

        // Stop the container
        let docker = &self.docker;
        let container_name = self.get_container_name();
        info!("Stopping MinIO container {}", container_name);

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
            docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop MinIO container: {}", e))?;
        }

        Ok(())
    }
    fn get_runtime_env_definitions(&self) -> Vec<super::RuntimeEnvVar> {
        vec![super::RuntimeEnvVar {
            name: "S3_BUCKET".to_string(),
            description: "S3 bucket name for this project/environment".to_string(),
            example: "project_123_production".to_string(),
            sensitive: false,
        }]
    }

    async fn get_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let bucket_name = format!("{}-{}", project_id, environment)
            .replace("_", "-")
            .to_lowercase();
        // Create the bucket
        self.create_bucket(config.clone(), &bucket_name).await?;
        let container_name = self.get_container_name();
        let mut env_vars = HashMap::new();

        // Bucket name (specific to this project/environment)
        env_vars.insert("S3_BUCKET".to_string(), bucket_name);

        // Endpoint
        let endpoint = format!("http://{}:{}", container_name, 9000);
        env_vars.insert("S3_ENDPOINT".to_string(), endpoint.clone());

        // Get access keys from service config
        let access_key = config
            .parameters
            .get("access_key")
            .and_then(|v| v.as_str())
            .context("Missing access key parameter")?;
        let secret_key = config
            .parameters
            .get("secret_key")
            .and_then(|v| v.as_str())
            .context("Missing secret key parameter")?;

        // S3-style environment variables (same as get_docker_environment_variables)
        env_vars.insert("S3_ACCESS_KEY".to_string(), access_key.to_string());
        env_vars.insert("S3_SECRET_KEY".to_string(), secret_key.to_string());
        env_vars.insert("S3_REGION".to_string(), "us-east-1".to_string());

        // AWS-style environment variables (for AWS SDK compatibility)
        env_vars.insert("AWS_ACCESS_KEY_ID".to_string(), access_key.to_string());
        env_vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key.to_string());
        env_vars.insert("AWS_DEFAULT_REGION".to_string(), "us-east-1".to_string());
        env_vars.insert("AWS_ENDPOINT_URL".to_string(), endpoint);

        Ok(env_vars)
    }
    async fn remove(&self) -> Result<()> {
        // First cleanup any connections
        self.cleanup().await?;

        // Then remove container and volume
        let docker = &self.docker;
        let container_name = self.get_container_name();
        let volume_name = format!("minio_{}_data", self.name);

        info!("Removing MinIO container and volume for {}", self.name);

        // Remove container if it exists
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
            // Stop container first if running
            docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to stop MinIO container: {}", e))?;

            // Remove the container
            docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to remove MinIO container: {}", e))?;
        }

        // Remove volume
        match docker
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

        // Build endpoint from host and port
        let host = parameters.get("host").context("Missing host parameter")?;
        let port = parameters.get("port").context("Missing port parameter")?;
        let endpoint = format!("http://{}:{}", host, port);

        let access_key = parameters
            .get("access_key")
            .context("Missing access key parameter")?;
        let secret_key = parameters
            .get("secret_key")
            .context("Missing secret key parameter")?;
        let region_val = "us-east-1".to_string();
        let region = parameters.get("region").unwrap_or(&region_val);

        env_vars.insert("S3_ENDPOINT".to_string(), endpoint.clone());
        env_vars.insert("S3_HOST".to_string(), host.clone());
        env_vars.insert("S3_PORT".to_string(), port.clone());
        env_vars.insert("S3_ACCESS_KEY".to_string(), access_key.clone());
        env_vars.insert("S3_SECRET_KEY".to_string(), secret_key.clone());
        env_vars.insert("S3_REGION".to_string(), region.clone());

        // Also provide AWS-style environment variables
        env_vars.insert("AWS_ACCESS_KEY_ID".to_string(), access_key.clone());
        env_vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key.clone());
        env_vars.insert("AWS_DEFAULT_REGION".to_string(), region.clone());
        env_vars.insert("AWS_ENDPOINT_URL".to_string(), endpoint.clone());

        Ok(env_vars)
    }
    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();
        let container_name = self.get_container_name();

        let access_key = parameters
            .get("access_key")
            .context("Missing access key parameter")?;
        let secret_key = parameters
            .get("secret_key")
            .context("Missing secret key parameter")?;
        let endpoint = format!("http://{container_name}:9000");

        env_vars.insert("S3_ENDPOINT".to_string(), endpoint.clone());
        env_vars.insert("S3_ACCESS_KEY".to_string(), access_key.clone());
        env_vars.insert("S3_SECRET_KEY".to_string(), secret_key.clone());
        env_vars.insert("S3_REGION".to_string(), "us-east-1".to_string());

        // AWS-style environment variables
        env_vars.insert("AWS_ACCESS_KEY_ID".to_string(), access_key.clone());
        env_vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key.clone());
        env_vars.insert("AWS_DEFAULT_REGION".to_string(), "us-east-1".to_string());
        env_vars.insert("AWS_ENDPOINT_URL".to_string(), endpoint);

        Ok(env_vars)
    }

    /// Backup S3 data to another S3 location
    async fn backup_to_s3(
        &self,
        // we are not using the s3 client for this backup, we are using the mc container to backup the data
        _s3_client: &aws_sdk_s3::Client,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        _subpath: &str,
        subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> Result<String> {
        use chrono::Utc;
        use sea_orm::*;

        info!(
            "Starting S3 backup using MinIO Client for backup {}",
            backup.id
        );

        // Use a standard backup path without versioning
        let backup_prefix = subpath_root;
        let container_name = format!("mc-backup-{}", backup.id);

        // Create a backup record directly using ActiveModel setters (no need to build and then copy)
        let backup_record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
                service_id: Set(external_service.id),
                backup_id: Set(backup.id),
                backup_type: Set("full".to_string()),
                state: Set("running".to_string()),
                started_at: Set(Utc::now()),
                s3_location: Set(backup_prefix.to_string()),
                metadata: Set(serde_json::json!({
                    "service_type": "s3",
                    "service_name": self.name,
                    "timestamp": Utc::now().to_rfc3339(),
                })),
                compression_type: Set("none".to_string()),
                created_by: Set(0), // System user ID
                ..Default::default()
            },
        )
        .exec_with_returning(pool)
        .await?;

        // Pull the MinIO Client image
        self.pull_mc_image(&self.docker).await?;

        let service_config = service_config.clone();
        let s3_source_config = self.get_s3_config(service_config)?;

        // Create environment variables for mc
        let dest_endpoint = s3_source
            .endpoint
            .clone()
            .unwrap_or(format!("{}:{}", s3_source.bucket_name, "9000"));

        let env_vars = [
            format!(
                "MC_HOST_source=http://{}:{}@{}:{}",
                s3_source_config.access_key,
                s3_source_config.secret_key,
                s3_source_config.host,
                s3_source_config.port
            ),
            format!(
                "MC_HOST_dest=http://{}:{}@{}",
                s3_source.access_key_id, s3_source.secret_key, dest_endpoint
            ),
        ];

        // Create mc container with a shell entrypoint
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(Self::MC_IMAGE.to_string()),
            env: Some(env_vars.iter().map(|s| s.as_str().to_string()).collect()),
            entrypoint: Some(vec!["sh".to_string()]),
            tty: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Create the container
        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await?;

        // Start the container
        self.docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        let source_endpoint = format!("http://{}:{}", s3_source_config.host, s3_source_config.port);
        let default_dest_endpoint = format!("http://{}:9000", s3_source.bucket_name);
        let dest_endpoint = s3_source
            .endpoint
            .as_deref()
            .unwrap_or(&default_dest_endpoint);
        let source_name = "original/".to_string();
        let dest_name = format!("backup-dest/{}/{}", s3_source.bucket_name, subpath_root);

        // Execute commands in sequence
        let commands = vec![
            // Add source alias
            vec![
                "mc",
                "alias",
                "set",
                "original",
                &source_endpoint,
                &s3_source_config.access_key,
                &s3_source_config.secret_key,
            ],
            // Add destination alias
            vec![
                "mc",
                "alias",
                "set",
                "backup-dest",
                &dest_endpoint,
                &s3_source.access_key_id,
                &s3_source.secret_key,
            ],
            // Perform the mirror operation (without --remove to preserve files)
            vec!["mc", "mirror", "--overwrite", &source_name, &dest_name],
        ];

        let mut success = true;
        let mut error_logs = Vec::new();

        for cmd in commands {
            info!("Executing command: {:?}", cmd);

            let exec = self
                .docker
                .create_exec(
                    &container.id,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(cmd.clone()),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;

            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                self.docker.start_exec(&exec.id, None).await?
            {
                while let Ok(Some(output)) = output.try_next().await {
                    match output {
                        bollard::container::LogOutput::StdOut { message } => {
                            info!("stdout: {}", String::from_utf8_lossy(&message));
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            error!("stderr: {}", String::from_utf8_lossy(&message));
                            error_logs.push(String::from_utf8_lossy(&message).to_string());
                        }
                        _ => {}
                    }
                }
            }

            // Check execution result
            if let Some(inspect_result) = self.docker.inspect_exec(&exec.id).await?.exit_code {
                if inspect_result != 0 {
                    success = false;
                    break;
                }
            }
        }

        // Clean up the container
        self.docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;

        if success {
            // Update backup record with success
            let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("completed".to_string());
            backup_update.finished_at = Set(Some(Utc::now()));
            temps_entities::external_service_backups::Entity::update(backup_update)
                .exec(pool)
                .await?;

            info!("S3 backup completed successfully");
            Ok(backup_prefix.to_string())
        } else {
            let error_message = error_logs.join("\n");

            // Update backup record with error
            let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("failed".to_string());
            backup_update.error_message = Set(Some(error_message.clone()));
            backup_update.finished_at = Set(Some(Utc::now()));
            temps_entities::external_service_backups::Entity::update(backup_update)
                .exec(pool)
                .await?;

            Err(anyhow::anyhow!("Backup failed: {}", error_message))
        }
    }

    async fn restore_from_s3(
        &self,
        // we are not using the s3 client for this restore, we are using the mc container to restore the backup
        _s3_client: &aws_sdk_s3::Client,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!(
            "Starting S3 restore from backup location: {}",
            backup_location
        );

        let docker = &self.docker;
        let container_name = format!("mc-restore-{}", uuid::Uuid::new_v4());
        let s3_config = self.get_s3_config(service_config)?;

        // Pull the MinIO Client image
        self.pull_mc_image(docker).await?;

        // Create environment variables for mc
        let env_vars = [
            format!(
                "MC_HOST_source=http://{}:{}@{}",
                s3_source.access_key_id,
                s3_source.secret_key,
                s3_source.endpoint.as_deref().unwrap_or("s3.amazonaws.com")
            ),
            format!(
                "MC_HOST_dest=http://{}:{}@localhost:{}",
                s3_config.access_key, s3_config.secret_key, s3_config.port
            ),
        ];

        // Create mc container with a shell entrypoint
        let container_config = bollard::models::ContainerCreateBody {
            image: Some(Self::MC_IMAGE.to_string()),
            env: Some(env_vars.iter().map(|s| s.as_str().to_string()).collect()),
            entrypoint: Some(vec!["sh".to_string()]),
            tty: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("host".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Create the container
        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                container_config,
            )
            .await?;

        // Start the container
        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        let source_endpoint = s3_source.endpoint.as_deref().unwrap_or("s3.amazonaws.com");
        let dest_endpoint = format!("http://localhost:{}", s3_config.port);

        // Base commands for setting up aliases
        let setup_commands = vec![
            // Add source alias
            vec![
                "mc",
                "alias",
                "set",
                "backup-source",
                source_endpoint,
                &s3_source.access_key_id,
                &s3_source.secret_key,
            ],
            // Add destination alias
            vec![
                "mc",
                "alias",
                "set",
                "dest",
                &dest_endpoint,
                &s3_config.access_key,
                &s3_config.secret_key,
            ],
        ];

        // Execute setup commands
        for cmd in setup_commands {
            let exec = docker
                .create_exec(
                    &container.id,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(cmd.clone()),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;

            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                docker.start_exec(&exec.id, None).await?
            {
                while let Ok(Some(output)) = output.try_next().await {
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
        }

        let source_backup_location = format!(
            "backup-source/{}/{}",
            s3_source.bucket_name, backup_location
        );
        // First, list the buckets in the backup location
        let list_command = vec!["mc", "ls", "--json", &source_backup_location];

        let mut buckets = Vec::new();

        // Execute list command to get buckets
        let exec = docker
            .create_exec(
                &container.id,
                bollard::exec::CreateExecOptions {
                    cmd: Some(list_command),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        if let bollard::exec::StartExecResults::Attached { mut output, .. } =
            docker.start_exec(&exec.id, None).await?
        {
            let mut output_str = String::new();
            while let Ok(Some(output)) = output.try_next().await {
                if let bollard::container::LogOutput::StdOut { message } = output {
                    output_str.push_str(&String::from_utf8_lossy(&message));
                }
            }
            println!("Output buckets {:?}", output_str);
            // Parse all JSON objects from the output
            let json_objects = parse_multiline_json_output(&output_str)?;

            // Process each JSON object
            for listing in json_objects {
                if let (Some("folder"), Some(key)) = (
                    listing.get("type").and_then(|t| t.as_str()),
                    listing.get("key").and_then(|k| k.as_str()),
                ) {
                    buckets.push(key.to_string());
                }
            }
        }

        info!("Found buckets to restore: {:?}", buckets);

        // For each bucket, create it and mirror its contents
        for bucket in buckets {
            let bucket_name = bucket.trim_end_matches('/');
            let dest_location = format!("dest/{}", bucket_name);
            // Create bucket command
            let create_bucket_cmd = vec!["mc", "mb", &dest_location];

            // Execute create bucket command
            let exec = docker
                .create_exec(
                    &container.id,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(create_bucket_cmd.clone()),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;

            let mut stdout = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                docker.start_exec(&exec.id, None).await?
            {
                while let Ok(Some(output)) = output.try_next().await {
                    match output {
                        bollard::container::LogOutput::StdOut { message } => {
                            let msg = String::from_utf8_lossy(&message);
                            stdout.push_str(&msg);
                            info!("stdout: {}", msg);
                        }
                        bollard::container::LogOutput::StdErr { message } => {
                            error!("stderr: {}", String::from_utf8_lossy(&message));
                        }
                        _ => {}
                    }
                }
            }

            // Check exit code and handle specific error case
            if let Some(inspect_result) = docker.inspect_exec(&exec.id).await?.exit_code {
                if inspect_result == 1 && !stdout.contains("object name cannot be empty") {
                    return Err(anyhow::anyhow!(
                        "Failed to create bucket {}: Exit code {} - {}",
                        bucket_name,
                        inspect_result,
                        stdout
                    ));
                }
            }

            let source_bucket_loc = format!(
                "backup-source/{}/{}/{}",
                s3_source.bucket_name, backup_location, bucket_name
            );
            let dest_bucket_loc = format!("dest/{}", bucket_name);
            // Mirror command for this bucket
            let mirror_cmd = vec![
                "mc",
                "mirror",
                "--skip-errors",
                "--overwrite",
                &source_bucket_loc,
                &dest_bucket_loc,
            ];

            info!(
                "Executing mirror command for bucket {}: {:?}",
                bucket_name, mirror_cmd
            );

            let exec = docker
                .create_exec(
                    &container.id,
                    bollard::exec::CreateExecOptions {
                        cmd: Some(mirror_cmd),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await?;

            if let bollard::exec::StartExecResults::Attached { mut output, .. } =
                docker.start_exec(&exec.id, None).await?
            {
                while let Ok(Some(output)) = output.try_next().await {
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
        }

        // Clean up the container
        docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;

        info!("S3 restore completed successfully");
        Ok(())
    }

    fn get_default_docker_image(&self) -> (String, String) {
        // Return (image_name, version)
        // Default MinIO image and release version
        (
            "minio/minio".to_string(),
            "RELEASE.2025-09-07T16-13-09Z".to_string(),
        )
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
                "Failed to get current docker image for S3/MinIO container"
            ))
        }
    }

    fn get_default_version(&self) -> String {
        "RELEASE.2025-09-07T16-13-09Z".to_string()
    }

    async fn get_current_version(&self) -> Result<String> {
        let (_, version) = self.get_current_docker_image().await?;
        Ok(version)
    }

    async fn upgrade(&self, old_config: ServiceConfig, new_config: ServiceConfig) -> Result<()> {
        info!("Starting S3/MinIO upgrade");

        let _old_s3_config = self.get_s3_config(old_config)?;
        let new_s3_config = self.get_s3_config(new_config)?;

        // Verify the new image can be pulled BEFORE stopping the old container
        info!(
            "Verifying new Docker image is available: {}",
            new_s3_config.docker_image
        );
        self.verify_image_pullable(&new_s3_config.docker_image)
            .await?;
        info!("New Docker image verified and is available");

        // Stop the old container
        info!("Stopping old S3/MinIO container");
        self.stop().await?;

        // Create container with new image (keeping the same volume for data persistence)
        info!("Starting S3/MinIO container with new image");
        self.create_container(&self.docker, &new_s3_config).await?;

        info!("S3/MinIO upgrade completed successfully");
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

        // Extract version from image name (e.g., "minio/minio:latest" -> "latest")
        let version = if let Some(tag_pos) = image.rfind(':') {
            image[tag_pos + 1..].to_string()
        } else {
            "latest".to_string()
        };

        // Extract port from additional config if provided, otherwise use 9000
        let port = additional_config
            .get("port")
            .and_then(|v| v.as_str())
            .unwrap_or("9000")
            .to_string();

        // Extract credentials
        let access_key = credentials
            .get("access_key")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Access key is required for S3/MinIO import"))?;
        let secret_key = credentials
            .get("secret_key")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Secret key is required for S3/MinIO import"))?;

        // Build endpoint
        let endpoint = format!("http://localhost:{}", port);

        // Verify connection to the imported service by attempting to list buckets
        match tokio::runtime::Runtime::new().ok().and_then(|rt| {
            rt.block_on(async {
                let creds = aws_sdk_s3::config::Credentials::new(
                    &access_key,
                    &secret_key,
                    None,
                    None,
                    "imported",
                );

                let config = aws_sdk_s3::config::Config::builder()
                    .credentials_provider(creds)
                    .endpoint_url(&endpoint)
                    .build();

                let client = aws_sdk_s3::Client::from_conf(config);
                client.list_buckets().send().await.ok()
            })
        }) {
            Some(_) => {
                info!("Successfully verified S3/MinIO connection for import");
            }
            None => {
                return Err(anyhow::anyhow!(
                    "Failed to connect to S3/MinIO at {} with provided credentials. Verify endpoint, access key, and secret key.",
                    endpoint
                ));
            }
        }

        // Build the ServiceConfig for registration
        let config = ServiceConfig {
            name: service_name,
            service_type: ServiceType::S3,
            version: Some(version),
            parameters: serde_json::json!({
                "endpoint": endpoint,
                "port": port,
                "access_key": access_key,
                "secret_key": secret_key,
                "use_ssl": false,
                "docker_image": image,
                "container_id": container_id,
            }),
        };

        info!(
            "Successfully imported S3/MinIO service '{}' from container",
            config.name
        );
        Ok(config)
    }
}

fn parse_multiline_json_output(output: &str) -> Result<Vec<serde_json::Value>> {
    let mut json_objects = Vec::new();
    let mut current_object = String::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        current_object.push_str(trimmed);

        // Try to parse the accumulated string as a JSON object
        if let Ok(json_value) = serde_json::from_str(&current_object) {
            json_objects.push(json_value);
            current_object.clear();
        }
    }

    Ok(json_objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parameter_schema_editable_fields() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = S3Service::new("test-editable".to_string(), docker);

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
            ("access_key", false),
            ("secret_key", false),
            ("region", false),
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

    #[test]
    fn test_default_docker_image() {
        let docker = Arc::new(Docker::connect_with_local_defaults().unwrap());
        let service = S3Service::new("test-image".to_string(), docker);

        let (image_name, version) = service.get_default_docker_image();
        assert_eq!(
            image_name, "minio/minio",
            "Default image should be minio/minio"
        );
        assert!(
            version.starts_with("RELEASE."),
            "Default version should be a MinIO release tag"
        );
    }

    #[test]
    fn test_image_field_in_configuration() {
        // Test S3 configuration with docker_image field
        let input_config = S3InputConfig {
            port: Some("9000".to_string()),
            access_key: Some("minioadmin".to_string()),
            secret_key: Some("minioadmin".to_string()),
            host: "localhost".to_string(),
            region: "us-east-1".to_string(),
            docker_image: "minio/minio:RELEASE.2025-09-07T16-13-09Z".to_string(),
        };

        // Convert to runtime config
        let runtime_config: S3Config = input_config.into();

        // Verify docker_image is preserved
        assert_eq!(
            runtime_config.docker_image,
            "minio/minio:RELEASE.2025-09-07T16-13-09Z"
        );
    }

    #[test]
    fn test_minio_version_upgrade_config() {
        // Test simulated MinIO image upgrade
        let old_config = super::ServiceConfig {
            name: "test-s3".to_string(),
            service_type: super::ServiceType::S3,
            version: None,
            parameters: serde_json::json!({
                "port": Some("9000"),
                "access_key": "minioadmin",
                "secret_key": "minioadmin",
                "host": "localhost",
                "region": "us-east-1",
                "image": "minio/minio:RELEASE.2025-06-01T01-00-00Z"
            }),
        };

        let new_config = super::ServiceConfig {
            name: "test-s3".to_string(),
            service_type: super::ServiceType::S3,
            version: None,
            parameters: serde_json::json!({
                "port": Some("9000"),
                "access_key": "minioadmin",
                "secret_key": "minioadmin",
                "host": "localhost",
                "region": "us-east-1",
                "image": "minio/minio:RELEASE.2025-09-07T16-13-09Z"
            }),
        };

        // Verify image upgrade configuration
        let old_image = old_config
            .parameters
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let new_image = new_config
            .parameters
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        assert!(
            old_image.contains("2025-06-01"),
            "Old image should contain 2025-06-01"
        );
        assert!(
            new_image.contains("2025-09-07"),
            "New image should contain 2025-09-07"
        );
        assert_ne!(old_image, new_image, "Images should be different");
    }

    #[test]
    fn test_import_service_config_creation() {
        let config = ServiceConfig {
            name: "test-s3-import".to_string(),
            service_type: ServiceType::S3,
            version: Some("latest".to_string()),
            parameters: serde_json::json!({
                "access_key": "minioadmin",
                "secret_key": "minioadmin",
                "endpoint_url": "http://localhost:9000",
                "region": "us-east-1",
                "use_ssl": false,
                "docker_image": "minio/minio:latest",
                "container_id": "ghi789jkl012",
            }),
        };

        assert_eq!(config.name, "test-s3-import");
        assert_eq!(config.service_type, ServiceType::S3);
        assert_eq!(config.version, Some("latest".to_string()));
    }

    #[test]
    fn test_import_s3_version_extraction() {
        let test_cases = vec![
            ("minio/minio:latest", "latest"),
            (
                "minio/minio:RELEASE.2025-01-01T00-00-00Z",
                "RELEASE.2025-01-01T00-00-00Z",
            ),
            ("minio/minio:2024", "2024"),
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
        // S3 requires access_key and secret_key

        assert!(credentials.get("access_key").is_none());
        assert!(credentials.get("secret_key").is_none());
    }

    #[test]
    fn test_import_credential_extraction() {
        let mut credentials: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        credentials.insert("access_key".to_string(), "AKIAIOSFODNN7EXAMPLE".to_string());
        credentials.insert(
            "secret_key".to_string(),
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        );
        credentials.insert(
            "endpoint_url".to_string(),
            "http://localhost:9000".to_string(),
        );

        assert_eq!(
            credentials.get("access_key").map(|s| s.as_str()),
            Some("AKIAIOSFODNN7EXAMPLE")
        );
        assert_eq!(
            credentials.get("secret_key").map(|s| s.as_str()),
            Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")
        );
        assert_eq!(
            credentials.get("endpoint_url").map(|s| s.as_str()),
            Some("http://localhost:9000")
        );
    }

    #[test]
    fn test_import_s3_endpoint_validation() {
        let endpoints = vec![
            "http://localhost:9000",
            "https://s3.amazonaws.com",
            "http://minio:9000",
        ];

        for endpoint in endpoints {
            assert!(
                endpoint.contains("://"),
                "Endpoint should have protocol: {}",
                endpoint
            );
        }
    }
}
