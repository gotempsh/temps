use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::Client;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::Docker;
use futures::TryStreamExt;
use http::Uri;
use rand::{distributions::Alphanumeric, Rng};
use sea_orm::prelude::*;
use serde::Deserialize;
use serde_json::{self};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info};

use crate::utils::ensure_network_exists;

use super::{ExternalService, ServiceConfig, ServiceParameter, ServiceType};

#[derive(Debug, Clone, Deserialize)]
pub struct S3Config {
    pub port: String,
    #[serde(default = "default_access_key")]
    pub access_key: String,
    #[serde(default = "default_secret_key")]
    pub secret_key: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_region")]
    pub region: String,
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

pub struct S3Service {
    name: String,
    config: Arc<RwLock<Option<S3Config>>>,
    client: Arc<RwLock<Option<Client>>>,
    docker: Arc<Docker>,
}

impl S3Service {
    const IMAGE: &'static str = "minio/minio:RELEASE.2025-09-07T16-13-09Z";
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
        info!("Pulling MinIO image {}", Self::IMAGE);

        // Parse image name and tag
        let (image_name, tag) = if let Some((name, tag)) = Self::IMAGE.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (Self::IMAGE.to_string(), "latest".to_string())
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
            image: Some(Self::IMAGE.to_string()),
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
            .context("Failed to start MinIO container")?;

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

    fn get_default_port(&self) -> String {
        // Start with default MinIO port
        let start_port = 9000;

        // Find next available port
        match find_available_port(start_port) {
            Some(port) => port.to_string(),
            None => start_port.to_string(), // Fallback to default if no ports found
        }
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
                    .context("Failed to delete bucket")?;

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
        let port = service_config
            .parameters
            .get("port")
            .context("Missing port parameter")?;
        let host = service_config
            .parameters
            .get("host")
            .context("Missing host parameter")?;
        let access_key = service_config
            .parameters
            .get("access_key")
            .context("Missing access_key parameter")?;
        let secret_key = service_config
            .parameters
            .get("secret_key")
            .context("Missing secret_key parameter")?;
        let region = service_config
            .parameters
            .get("region")
            .context("Missing region parameter")?;

        Ok(S3Config {
            port: port.as_str().unwrap_or("9000").to_string(),
            host: host.as_str().unwrap_or("localhost").to_string(),
            access_key: access_key.as_str().unwrap_or("minio").to_string(),
            secret_key: secret_key.as_str().unwrap_or("minio123").to_string(),
            region: region.as_str().unwrap_or("us-east-1").to_string(),
        })
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
        let s3_config: S3Config = serde_json::from_value(config.parameters)
            .context("Failed to parse S3 configuration")?;
        info!("Initializing S3 config {:?}", s3_config);
        let mut inferred_params = HashMap::new();
        inferred_params.insert("host".to_string(), s3_config.host.clone());
        inferred_params.insert("port".to_string(), s3_config.port.clone());
        inferred_params.insert(
            "endpoint".to_string(),
            format!("http://{}:{}", s3_config.host, s3_config.port),
        );

        // Generate credentials if Docker is available
        inferred_params.insert("access_key".to_string(), s3_config.access_key.clone());
        inferred_params.insert("secret_key".to_string(), s3_config.secret_key.clone());
        inferred_params.insert("region".to_string(), s3_config.region.clone());

        // Create Docker container
        self.create_container(&self.docker, &s3_config).await?;

        *self.config.write().await = Some(s3_config);
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

    fn validate_parameters(&self, parameters: &HashMap<String, String>) -> Result<()> {
        // // Use default validation from trait
        // ExternalService::validate_parameters(self, parameters)?;

        // Additional S3-specific validation
        if let Some(endpoint) = parameters.get("endpoint") {
            endpoint
                .parse::<Uri>()
                .map_err(|_| anyhow::anyhow!("Invalid endpoint URL format"))?;
        }

        // Validate access key length
        if let Some(access_key) = parameters.get("access_key_id") {
            if access_key.len() < 3 {
                return Err(anyhow::anyhow!(
                    "Access key must be at least 3 characters long"
                ));
            }
        }

        // Validate secret key length
        if let Some(secret_key) = parameters.get("secret_access_key") {
            if secret_key.len() < 8 {
                return Err(anyhow::anyhow!(
                    "Secret access key must be at least 8 characters long"
                ));
            }
        }

        Ok(())
    }

    fn get_parameter_definitions(&self) -> Vec<ServiceParameter> {
        vec![ServiceParameter {
            name: "port".to_string(),
            required: true,
            encrypted: false,
            description: "Port to expose MinIO service".to_string(),
            default_value: Some(self.get_default_port()),
            validation_pattern: Some(r"^\d+$".to_string()),
            choices: None,
        }]
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
                .context("Failed to start existing MinIO container")?;
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
                .context("Failed to stop MinIO container")?;
        }

        Ok(())
    }
    fn get_runtime_env_definitions(&self) -> Vec<super::RuntimeEnvVar> {
        vec![super::RuntimeEnvVar {
            name: "S3_BUCKET".to_string(),
            description: "S3 bucket name for this project/environment".to_string(),
            example: "project_123_production".to_string(),
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
                .context("Failed to stop MinIO container")?;

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
                .context("Failed to remove MinIO container")?;
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

        let endpoint = parameters
            .get("endpoint")
            .context("Missing endpoint parameter")?;
        let access_key = parameters
            .get("access_key")
            .context("Missing access key parameter")?;
        let secret_key = parameters
            .get("secret_key")
            .context("Missing secret key parameter")?;
        let region_val = "us-east-1".to_string();
        let region = parameters.get("region").unwrap_or(&region_val);

        env_vars.insert("S3_ENDPOINT".to_string(), endpoint.clone());
        env_vars.insert("S3_ACCESS_KEY".to_string(), access_key.clone());
        env_vars.insert("S3_SECRET_KEY".to_string(), secret_key.clone());
        env_vars.insert("S3_REGION".to_string(), region.clone());

        // Also provide AWS-style environment variables
        env_vars.insert("AWS_ACCESS_KEY_ID".to_string(), access_key.clone());
        env_vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key.clone());
        env_vars.insert("AWS_DEFAULT_REGION".to_string(), region.clone());

        if !endpoint.contains("amazonaws.com") {
            env_vars.insert("AWS_ENDPOINT_URL".to_string(), endpoint.clone());
        }

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
        let container_name = format!("mc_backup_{}", backup.id);

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
        let container_name = format!("mc_restore_{}", uuid::Uuid::new_v4());
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
}

fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn find_available_port(start_port: u16) -> Option<u16> {
    (start_port..start_port + 100).find(|&port| is_port_available(port))
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
