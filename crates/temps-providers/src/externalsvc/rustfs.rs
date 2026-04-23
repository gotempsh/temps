//! RustFS Service implementation
//!
//! RustFS is a high-performance, distributed object storage system built in Rust.
//! It provides S3-compatible API and is 2.3x faster than MinIO for small object payloads.
//!
//! See: https://github.com/rustfs/rustfs

use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::Client;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::Docker;
use futures::TryStreamExt;
use rand::Rng;
use schemars::JsonSchema;
use sea_orm::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{self};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use temps_core::EncryptionService;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info};

use crate::utils::ensure_network_exists;

use super::{ExternalService, HealthProbeResult, ServiceConfig, ServiceType};

/// Default RustFS Docker image (from Docker Hub)
pub const DEFAULT_RUSTFS_IMAGE: &str = "rustfs/rustfs:1.0.0-alpha.98";
/// Default RustFS API port
pub const DEFAULT_RUSTFS_API_PORT: u16 = 9000;
/// Default RustFS console port
pub const DEFAULT_RUSTFS_CONSOLE_PORT: u16 = 9001;
/// Default RustFS username
pub const DEFAULT_RUSTFS_USER: &str = "rustfsadmin";
/// Default RustFS password
pub const DEFAULT_RUSTFS_PASSWORD: &str = "rustfsadmin";

/// Input configuration for creating a RustFS service
/// This is what users provide when creating the service
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "RustFS Configuration",
    description = "Configuration for RustFS S3-compatible storage service"
)]
pub struct RustfsInputConfig {
    /// RustFS API port (auto-assigned if not provided)
    #[schemars(example = "example_port")]
    pub port: Option<String>,

    /// RustFS console port (auto-assigned if not provided)
    #[schemars(example = "example_console_port")]
    pub console_port: Option<String>,

    /// Access key (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_key")]
    #[schemars(with = "Option<String>", example = "example_access_key")]
    pub access_key: Option<String>,

    /// Secret key (auto-generated if not provided or empty)
    #[serde(default, deserialize_with = "deserialize_optional_key")]
    #[schemars(with = "Option<String>", example = "example_secret_key")]
    pub secret_key: Option<String>,

    /// Host address
    #[serde(default = "default_host")]
    #[schemars(example = "example_host", default = "default_host")]
    pub host: String,

    /// S3 region
    #[serde(default = "default_region")]
    #[schemars(example = "example_region", default = "default_region")]
    pub region: String,

    /// Docker image to use for RustFS
    #[serde(default = "default_image")]
    #[schemars(example = "example_image", default = "default_image")]
    pub docker_image: String,
}

/// Internal runtime configuration for RustFS service
/// This is what the service uses internally after processing input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustfsConfig {
    pub port: String,
    pub console_port: String,
    pub access_key: String,
    pub secret_key: String,
    pub host: String,
    pub region: String,
    pub docker_image: String,
}

impl RustfsConfig {
    /// Create a RustfsConfig from input, using async Docker-aware port finding
    /// Even if ports are provided, validates they are available and finds new ones if not
    async fn from_input_async(input: RustfsInputConfig, docker: &Docker) -> Self {
        // For API port: use provided if available, otherwise find a new one
        let port = match &input.port {
            Some(p) => {
                let port_num: u16 = p.parse().unwrap_or(DEFAULT_RUSTFS_API_PORT);
                // Check if the provided port is actually available
                if is_port_available_async(docker, port_num).await {
                    p.clone()
                } else {
                    // Port is in use, find a new one
                    tracing::warn!(
                        "Provided port {} is not available, finding a new one",
                        port_num
                    );
                    find_available_port_async(docker, DEFAULT_RUSTFS_API_PORT)
                        .await
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| DEFAULT_RUSTFS_API_PORT.to_string())
                }
            }
            None => find_available_port_async(docker, DEFAULT_RUSTFS_API_PORT)
                .await
                .map(|p| p.to_string())
                .unwrap_or_else(|| DEFAULT_RUSTFS_API_PORT.to_string()),
        };

        // For console port, start searching after the API port to avoid conflicts
        let api_port: u16 = port.parse().unwrap_or(DEFAULT_RUSTFS_API_PORT);
        let console_start = std::cmp::max(api_port + 1, DEFAULT_RUSTFS_CONSOLE_PORT);

        let console_port = match &input.console_port {
            Some(p) => {
                let port_num: u16 = p.parse().unwrap_or(DEFAULT_RUSTFS_CONSOLE_PORT);
                // Check if the provided port is actually available
                if is_port_available_async(docker, port_num).await {
                    p.clone()
                } else {
                    // Port is in use, find a new one
                    tracing::warn!(
                        "Provided console port {} is not available, finding a new one",
                        port_num
                    );
                    find_available_port_async(docker, console_start)
                        .await
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| DEFAULT_RUSTFS_CONSOLE_PORT.to_string())
                }
            }
            None => find_available_port_async(docker, console_start)
                .await
                .map(|p| p.to_string())
                .unwrap_or_else(|| DEFAULT_RUSTFS_CONSOLE_PORT.to_string()),
        };

        Self {
            port,
            console_port,
            access_key: input.access_key.unwrap_or_else(default_access_key),
            secret_key: input.secret_key.unwrap_or_else(default_secret_key),
            host: input.host,
            region: input.region,
            docker_image: input.docker_image,
        }
    }
}

impl From<RustfsInputConfig> for RustfsConfig {
    fn from(input: RustfsInputConfig) -> Self {
        Self {
            port: input.port.unwrap_or_else(|| {
                find_available_port(DEFAULT_RUSTFS_API_PORT)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| DEFAULT_RUSTFS_API_PORT.to_string())
            }),
            console_port: input.console_port.unwrap_or_else(|| {
                find_available_port(DEFAULT_RUSTFS_CONSOLE_PORT)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| DEFAULT_RUSTFS_CONSOLE_PORT.to_string())
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
    // AWS Access Key format: AKIA + 16 uppercase alphanumeric characters = 20 chars total
    let mut rng = rand::thread_rng();
    let random_part: String = (0..16)
        .map(|_| {
            let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            charset[rng.gen_range(0..charset.len())] as char
        })
        .collect();
    format!("AKIA{}", random_part)
}

fn default_secret_key() -> String {
    // AWS Secret Key format: 40 characters of base64-like characters (alphanumeric + / +)
    let mut rng = rand::thread_rng();
    (0..40)
        .map(|_| {
            let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789/+";
            charset[rng.gen_range(0..charset.len())] as char
        })
        .collect()
}

// Schema example functions
fn example_port() -> &'static str {
    "9000"
}

fn example_console_port() -> &'static str {
    "9001"
}

fn example_access_key() -> &'static str {
    "rustfsadmin"
}

fn example_secret_key() -> &'static str {
    "rustfsadmin"
}

fn example_host() -> &'static str {
    "localhost"
}

fn example_region() -> &'static str {
    "us-east-1"
}

fn default_image() -> String {
    DEFAULT_RUSTFS_IMAGE.to_string()
}

fn example_image() -> &'static str {
    "rustfs/rustfs:latest"
}

fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn find_available_port(start_port: u16) -> Option<u16> {
    (start_port..start_port + 1000).find(|&port| is_port_available(port))
}

/// Check if a specific port is available (both OS and Docker)
async fn is_port_available_async(docker: &Docker, port: u16) -> bool {
    // Check OS-level availability first
    if !is_port_available(port) {
        return false;
    }

    // Check Docker containers
    let containers = match docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await
    {
        Ok(c) => c,
        Err(_) => return true, // If we can't check Docker, assume available
    };

    for container in containers {
        if let Some(port_mappings) = container.ports {
            for port_mapping in port_mappings {
                if let Some(public_port) = port_mapping.public_port {
                    if public_port == port {
                        return false;
                    }
                }
            }
        }
    }

    true
}

/// Async version that checks both OS and Docker port availability
async fn find_available_port_async(docker: &Docker, start_port: u16) -> Option<u16> {
    // Get all ports currently used by Docker containers
    let docker_ports: std::collections::HashSet<u16> = {
        let containers = match docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                ..Default::default()
            }))
            .await
        {
            Ok(c) => c,
            Err(_) => return find_available_port(start_port), // Fallback to OS-only check
        };

        let mut ports = std::collections::HashSet::new();
        for container in containers {
            if let Some(port_mappings) = container.ports {
                for port_mapping in port_mappings {
                    if let Some(public_port) = port_mapping.public_port {
                        ports.insert(public_port);
                    }
                }
            }
        }
        ports
    };

    // Find a port that's available both at OS level and not used by Docker
    (start_port..start_port + 1000)
        .find(|&port| !docker_ports.contains(&port) && is_port_available(port))
}

pub struct RustfsService {
    name: String,
    config: Arc<RwLock<Option<RustfsConfig>>>,
    client: Arc<RwLock<Option<Client>>>,
    docker: Arc<Docker>,
    /// Reserved for encrypting/decrypting credentials when storing to database
    #[allow(dead_code)]
    encryption_service: Arc<EncryptionService>,
}

impl RustfsService {
    /// MinIO Client (mc) utility image - used for backup/restore operations via mc mirror
    const MC_IMAGE: &'static str = "minio/mc:RELEASE.2025-08-13T08-35-41Z";

    pub fn new(
        name: String,
        docker: Arc<Docker>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            name,
            config: Arc::new(RwLock::new(None)),
            client: Arc::new(RwLock::new(None)),
            docker,
            encryption_service,
        }
    }

    fn get_container_name(&self) -> String {
        format!("rustfs-{}", self.name)
    }

    /// Pull the MinIO Client (mc) image used for backup/restore operations
    async fn pull_mc_image(&self, docker: &Docker) -> Result<()> {
        info!("Pulling MinIO Client image {}", Self::MC_IMAGE);

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

    /// Parse ServiceConfig parameters into RustfsConfig
    fn get_rustfs_config(&self, service_config: ServiceConfig) -> Result<RustfsConfig> {
        let input_config: RustfsInputConfig = serde_json::from_value(service_config.parameters)
            .map_err(|e| anyhow::anyhow!("Failed to parse RustFS configuration: {}", e))?;

        Ok(RustfsConfig::from(input_config))
    }

    /// Execute a command in a container and return (success, stdout, stderr)
    async fn exec_in_container(
        &self,
        docker: &Docker,
        container_id: &str,
        cmd: Vec<&str>,
    ) -> Result<(bool, String, String)> {
        let exec = docker
            .create_exec(
                container_id,
                bollard::exec::CreateExecOptions {
                    cmd: Some(cmd.clone()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let bollard::exec::StartExecResults::Attached { mut output, .. } =
            docker.start_exec(&exec.id, None).await?
        {
            while let Ok(Some(output)) = output.try_next().await {
                match output {
                    bollard::container::LogOutput::StdOut { message } => {
                        let msg = String::from_utf8_lossy(&message);
                        info!("stdout: {}", msg);
                        stdout.push_str(&msg);
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        let msg = String::from_utf8_lossy(&message);
                        error!("stderr: {}", msg);
                        stderr.push_str(&msg);
                    }
                    _ => {}
                }
            }
        }

        let exit_code = docker.inspect_exec(&exec.id).await?.exit_code.unwrap_or(-1);

        Ok((exit_code == 0, stdout, stderr))
    }

    async fn create_container(&self, docker: &Docker, config: &RustfsConfig) -> Result<()> {
        // Pull the image first
        info!("Pulling RustFS image {}", config.docker_image);

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
        // Add volume names for data and logs
        let data_volume_name = format!("rustfs_{}_data", self.name);
        let logs_volume_name = format!("rustfs_{}_logs", self.name);

        // Create volumes if they don't exist
        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(data_volume_name.clone()),
                ..Default::default()
            })
            .await?;

        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(logs_volume_name.clone()),
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
            let container = containers.first().unwrap();
            let existing_image = container.image.as_deref().unwrap_or("");
            let is_running =
                container.state == Some(bollard::models::ContainerSummaryStateEnum::RUNNING);

            // Check if container is running with same image - if so, we're good
            if existing_image == config.docker_image && is_running {
                info!(
                    "Container {} already exists and is running with same image",
                    container_name
                );
                return Ok(());
            }

            // Container exists but is not running or has different image - remove and recreate
            info!(
                "Container {} exists (running: {}, image: {}) but needs to be recreated (requested image: {})",
                container_name, is_running, existing_image, config.docker_image
            );

            // Stop the container first (ignore errors if already stopped)
            let _ = docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await;

            // Remove the container
            docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await?;
        }

        let service_label_key = format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX);
        let name_label_key = format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX);

        let container_labels = HashMap::from([
            (service_label_key.as_str(), "rustfs"),
            (name_label_key.as_str(), self.name.as_str()),
        ]);

        // RustFS uses RUSTFS_ACCESS_KEY and RUSTFS_SECRET_KEY environment variables
        let env_vars = [
            format!("RUSTFS_ACCESS_KEY={}", config.access_key),
            format!("RUSTFS_SECRET_KEY={}", config.secret_key),
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
            port_bindings: Some(HashMap::from([
                (
                    "9000/tcp".to_string(),
                    Some(vec![bollard::models::PortBinding {
                        host_ip: Some("0.0.0.0".to_string()),
                        host_port: Some(config.port.to_string()),
                    }]),
                ),
                (
                    "9001/tcp".to_string(),
                    Some(vec![bollard::models::PortBinding {
                        host_ip: Some("0.0.0.0".to_string()),
                        host_port: Some(config.console_port.to_string()),
                    }]),
                ),
            ])),
            // Add volume mounts for data and logs
            mounts: Some(vec![
                bollard::models::Mount {
                    target: Some("/data".to_string()),
                    source: Some(data_volume_name.clone()),
                    typ: Some(bollard::models::MountTypeEnum::VOLUME),
                    ..Default::default()
                },
                bollard::models::Mount {
                    target: Some("/logs".to_string()),
                    source: Some(logs_volume_name.clone()),
                    typ: Some(bollard::models::MountTypeEnum::VOLUME),
                    ..Default::default()
                },
            ]),
            log_config: Some(crate::utils::default_service_log_config()),
            // Security hardening for service containers
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            pids_limit: Some(512),
            ..Default::default()
        };

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(config.docker_image.to_string()),
            networking_config,
            exposed_ports: Some(Vec::from(["9000/tcp".to_string(), "9001/tcp".to_string()])),
            env: Some(env_vars.iter().map(|s| s.as_str().to_string()).collect()),
            labels: Some(
                container_labels
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            host_config: Some(bollard::models::HostConfig {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..host_config
            }),
            // RustFS healthcheck - check if the health endpoint is responding
            healthcheck: Some(bollard::models::HealthConfig {
                test: Some(vec![
                    "CMD-SHELL".to_string(),
                    "curl -sf http://localhost:9000/health > /dev/null || exit 1".to_string(),
                ]),
                interval: Some(2000000000), // 2 seconds
                timeout: Some(5000000000),  // 5 seconds
                retries: Some(3),
                start_period: Some(10000000000),  // 10 seconds
                start_interval: Some(2000000000), // 2 seconds
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
            .map_err(|e| anyhow::anyhow!("Failed to create RustFS container: {}", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start RustFS container: {}", e))?;

        // Spawn health check as background task (non-blocking)
        let container_id = container.id.clone();
        let container_name_clone = container_name.clone();
        let docker_clone = docker.clone();
        tokio::spawn(async move {
            let mut delay = Duration::from_millis(100);
            let mut total_wait = Duration::from_secs(0);
            let max_wait = Duration::from_secs(60);

            while total_wait < max_wait {
                if let Ok(info) = docker_clone
                    .inspect_container(&container_id, None::<InspectContainerOptions>)
                    .await
                {
                    if let Some(state) = info.state {
                        if state.status == Some(bollard::models::ContainerStateStatusEnum::RUNNING)
                            && state.health.as_ref().and_then(|h| h.status.as_ref())
                                == Some(&bollard::models::HealthStatusEnum::HEALTHY)
                        {
                            info!("RustFS container {} is healthy", container_name_clone);
                            return;
                        }
                    }
                }
                sleep(delay).await;
                total_wait += delay;
                delay = delay.mul_f32(1.5);
            }
            error!(
                "RustFS container {} health check timed out after 60s",
                container_name_clone
            );
        });

        info!(
            "RustFS container {} created and started (health check running in background)",
            container.id
        );
        Ok(())
    }

    async fn create_s3_client(&self, config: &RustfsConfig) -> Result<Client> {
        let endpoint = format!("http://{}:{}", config.host, config.port);
        let credentials = aws_sdk_s3::config::Credentials::new(
            &config.access_key,
            &config.secret_key,
            None,
            None,
            "rustfs",
        );

        let sdk_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .endpoint_url(&endpoint)
            .force_path_style(true)
            .credentials_provider(credentials)
            .build();

        Ok(Client::from_conf(sdk_config))
    }

    /// Create a fresh S3 client connection
    /// Connection will be automatically closed when Client is dropped
    pub async fn get_connection(&self) -> Result<Client> {
        let config_guard = self.config.read().await;
        if let Some(config) = config_guard.as_ref() {
            self.create_s3_client(config).await
        } else {
            Err(anyhow::anyhow!("RustFS service not initialized"))
        }
    }

    /// Ensure the given bucket exists, creating it if missing.
    async fn ensure_bucket(&self, config: ServiceConfig, name: &str) -> Result<()> {
        let runtime_config: RustfsConfig = {
            let input_config: RustfsInputConfig = serde_json::from_value(config.parameters.clone())
                .context("Failed to parse RustFS configuration")?;
            RustfsConfig::from(input_config)
        };

        let client = self.create_s3_client(&runtime_config).await?;
        let sanitized_name = name.replace('_', "-").to_lowercase();

        match client.head_bucket().bucket(&sanitized_name).send().await {
            Ok(_) => {
                info!("RustFS bucket {} already exists", sanitized_name);
                return Ok(());
            }
            Err(err) => {
                tracing::debug!("RustFS bucket {} does not exist: {}", sanitized_name, err);
            }
        }

        client
            .create_bucket()
            .bucket(sanitized_name.clone())
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to create RustFS bucket {}: {:?}", sanitized_name, e)
            })?;

        info!("Created RustFS bucket {}", sanitized_name);
        Ok(())
    }
}

#[async_trait]
impl ExternalService for RustfsService {
    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!("Initializing RustFS service: {}", config.name);

        // Parse input configuration
        let input_config: RustfsInputConfig = serde_json::from_value(config.parameters.clone())
            .context("Failed to parse RustFS configuration")?;

        // Convert to runtime config using async Docker-aware port finding
        let runtime_config = RustfsConfig::from_input_async(input_config, &self.docker).await;

        // Create container
        self.create_container(&self.docker, &runtime_config).await?;

        // Create S3 client
        let client = self.create_s3_client(&runtime_config).await?;

        // Store configuration and client
        {
            let mut config_guard = self.config.write().await;
            *config_guard = Some(runtime_config.clone());
        }
        {
            let mut client_guard = self.client.write().await;
            *client_guard = Some(client);
        }

        // Return inferred parameters for storage
        let mut inferred = HashMap::new();
        inferred.insert("port".to_string(), runtime_config.port);
        inferred.insert("console_port".to_string(), runtime_config.console_port);
        inferred.insert("access_key".to_string(), runtime_config.access_key);
        inferred.insert("secret_key".to_string(), runtime_config.secret_key);
        inferred.insert("host".to_string(), runtime_config.host);
        inferred.insert("region".to_string(), runtime_config.region);
        inferred.insert("docker_image".to_string(), runtime_config.docker_image);

        Ok(inferred)
    }

    async fn health_check(&self) -> Result<bool> {
        let client_guard = self.client.read().await;
        if let Some(client) = client_guard.as_ref() {
            // Try to list buckets as a health check
            match client.list_buckets().send().await {
                Ok(_) => Ok(true),
                Err(e) => {
                    error!("RustFS health check failed: {}", e);
                    Ok(false)
                }
            }
        } else {
            Ok(false)
        }
    }

    async fn health_probe(&self, service_config: ServiceConfig) -> Result<HealthProbeResult> {
        use std::time::Instant;

        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
        const DEGRADED_MS: u128 = 2000;

        let cfg = match self.get_rustfs_config(service_config) {
            Ok(c) => c,
            Err(e) => {
                return Ok(HealthProbeResult::down(format!(
                    "invalid rustfs config: {}",
                    e
                )))
            }
        };

        let endpoint = format!("http://{}:{}", cfg.host, cfg.port);
        let start = Instant::now();

        let probe = async {
            let creds = aws_sdk_s3::config::Credentials::new(
                cfg.access_key.clone(),
                cfg.secret_key.clone(),
                None,
                None,
                "rustfs-health-probe",
            );
            let s3_config = aws_sdk_s3::Config::builder()
                .region(Region::new(cfg.region.clone()))
                .endpoint_url(endpoint.clone())
                .credentials_provider(creds)
                .force_path_style(true)
                .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                .build();
            let client = Client::from_conf(s3_config);
            client
                .list_buckets()
                .send()
                .await
                .map_err(|e| format!("ListBuckets failed: {}", e))?;
            Ok::<(), String>(())
        };

        match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            Err(_) => Ok(HealthProbeResult::down(format!(
                "rustfs probe to {} timed out after {}s",
                endpoint,
                PROBE_TIMEOUT.as_secs()
            ))),
            Ok(Err(msg)) => Ok(HealthProbeResult::down(format!(
                "rustfs probe to {} {}",
                endpoint, msg
            ))),
            Ok(Ok(())) => {
                let elapsed_ms = start.elapsed().as_millis();
                let response_time = i32::try_from(elapsed_ms).ok();
                if elapsed_ms > DEGRADED_MS {
                    Ok(HealthProbeResult::degraded(
                        format!("rustfs responded in {}ms (>{}ms)", elapsed_ms, DEGRADED_MS),
                        response_time,
                    ))
                } else {
                    Ok(HealthProbeResult::operational(response_time))
                }
            }
        }
    }

    fn get_type(&self) -> ServiceType {
        ServiceType::Blob
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_connection_info(&self) -> Result<String> {
        let config = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Config locked"))?;
        if let Some(cfg) = config.as_ref() {
            Ok(format!("http://{}:{}", cfg.host, cfg.port))
        } else {
            Err(anyhow::anyhow!("Service not initialized"))
        }
    }

    async fn cleanup(&self) -> Result<()> {
        self.stop().await?;
        self.remove().await
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        let schema = schemars::schema_for!(RustfsInputConfig);
        serde_json::to_value(schema).ok()
    }

    async fn start(&self) -> Result<()> {
        let container_name = self.get_container_name();
        self.docker
            .start_container(
                &container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start RustFS container: {}", e))?;
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let container_name = self.get_container_name();
        self.docker
            .stop_container(&container_name, None::<StopContainerOptions>)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to stop RustFS container: {}", e))?;
        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        let container_name = self.get_container_name();

        // Stop the container first
        let _ = self.stop().await;

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
            .map_err(|e| anyhow::anyhow!("Failed to remove RustFS container: {}", e))?;

        // Remove volumes
        let data_volume_name = format!("rustfs_{}_data", self.name);
        let logs_volume_name = format!("rustfs_{}_logs", self.name);

        let _ = self
            .docker
            .remove_volume(
                &data_volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;
        let _ = self
            .docker
            .remove_volume(
                &logs_volume_name,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;

        Ok(())
    }

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut env = HashMap::new();

        let host = parameters
            .get("host")
            .cloned()
            .unwrap_or_else(|| "localhost".to_string());
        let port = parameters
            .get("port")
            .cloned()
            .unwrap_or_else(|| "9000".to_string());
        let access_key = parameters
            .get("access_key")
            .cloned()
            .unwrap_or_else(|| "".to_string());
        let secret_key = parameters
            .get("secret_key")
            .cloned()
            .unwrap_or_else(|| "".to_string());
        let region = parameters
            .get("region")
            .cloned()
            .unwrap_or_else(|| "us-east-1".to_string());

        env.insert(
            "BLOB_ENDPOINT".to_string(),
            format!("http://{}:{}", host, port),
        );
        env.insert("BLOB_ACCESS_KEY".to_string(), access_key.clone());
        env.insert("BLOB_SECRET_KEY".to_string(), secret_key.clone());
        env.insert("BLOB_REGION".to_string(), region);

        // Also provide S3-compatible variable names
        env.insert(
            "S3_ENDPOINT".to_string(),
            format!("http://{}:{}", host, port),
        );
        env.insert("AWS_ACCESS_KEY_ID".to_string(), access_key);
        env.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key);

        Ok(env)
    }

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        // For Docker containers, use the container name as host
        let container_name = self.get_container_name();
        let port = parameters
            .get("port")
            .cloned()
            .unwrap_or_else(|| "9000".to_string());
        let access_key = parameters
            .get("access_key")
            .cloned()
            .unwrap_or_else(|| "".to_string());
        let secret_key = parameters
            .get("secret_key")
            .cloned()
            .unwrap_or_else(|| "".to_string());
        let region = parameters
            .get("region")
            .cloned()
            .unwrap_or_else(|| "us-east-1".to_string());

        let mut env = HashMap::new();
        env.insert(
            "BLOB_ENDPOINT".to_string(),
            format!("http://{}:{}", container_name, port),
        );
        env.insert("BLOB_ACCESS_KEY".to_string(), access_key.clone());
        env.insert("BLOB_SECRET_KEY".to_string(), secret_key.clone());
        env.insert("BLOB_REGION".to_string(), region);

        // Also provide S3-compatible variable names
        env.insert(
            "S3_ENDPOINT".to_string(),
            format!("http://{}:{}", container_name, port),
        );
        env.insert("AWS_ACCESS_KEY_ID".to_string(), access_key);
        env.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key);

        Ok(env)
    }

    fn get_runtime_env_definitions(&self) -> Vec<super::RuntimeEnvVar> {
        vec![
            super::RuntimeEnvVar {
                name: "S3_BUCKET".to_string(),
                description: "S3 bucket name for this project/environment".to_string(),
                example: "project-123-production".to_string(),
                sensitive: false,
            },
            super::RuntimeEnvVar {
                name: "S3_ENDPOINT".to_string(),
                description: "S3-compatible endpoint URL (internal container name)".to_string(),
                example: "http://rustfs-my-service:9000".to_string(),
                sensitive: false,
            },
        ]
    }

    async fn get_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let bucket_name = format!("{}-{}", project_id, environment)
            .replace('_', "-")
            .to_lowercase();

        self.ensure_bucket(config.clone(), &bucket_name).await?;

        let effective_host = self.get_container_name();
        let effective_port = DEFAULT_RUSTFS_API_PORT.to_string();
        let endpoint = format!("http://{}:{}", effective_host, effective_port);

        let access_key = config
            .parameters
            .get("access_key")
            .and_then(|v| v.as_str())
            .context("Missing RustFS access_key parameter")?;
        let secret_key = config
            .parameters
            .get("secret_key")
            .and_then(|v| v.as_str())
            .context("Missing RustFS secret_key parameter")?;
        let region = config
            .parameters
            .get("region")
            .and_then(|v| v.as_str())
            .unwrap_or("us-east-1");

        let mut env_vars = HashMap::new();

        env_vars.insert("S3_BUCKET".to_string(), bucket_name);
        env_vars.insert("S3_ENDPOINT".to_string(), endpoint.clone());
        env_vars.insert("S3_HOST".to_string(), effective_host.clone());
        env_vars.insert("S3_PORT".to_string(), effective_port);
        env_vars.insert("S3_ACCESS_KEY".to_string(), access_key.to_string());
        env_vars.insert("S3_SECRET_KEY".to_string(), secret_key.to_string());
        env_vars.insert("S3_REGION".to_string(), region.to_string());

        env_vars.insert("AWS_ACCESS_KEY_ID".to_string(), access_key.to_string());
        env_vars.insert("AWS_SECRET_ACCESS_KEY".to_string(), secret_key.to_string());
        env_vars.insert("AWS_DEFAULT_REGION".to_string(), region.to_string());
        env_vars.insert("AWS_ENDPOINT_URL".to_string(), endpoint);

        Ok(env_vars)
    }

    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let port: String = serde_json::from_value(
            service_config
                .parameters
                .get("port")
                .cloned()
                .unwrap_or(serde_json::Value::String("9000".to_string())),
        )
        .unwrap_or_else(|_| "9000".to_string());

        Ok(format!("localhost:{}", port))
    }

    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        let port: String = serde_json::from_value(
            service_config
                .parameters
                .get("port")
                .cloned()
                .unwrap_or(serde_json::Value::String("9000".to_string())),
        )
        .unwrap_or_else(|_| "9000".to_string());

        // In Docker mode, use container name
        let container_name = self.get_container_name();
        Ok((container_name, port))
    }

    fn get_docker_container_name(&self) -> String {
        self.get_container_name()
    }

    fn get_docker_internal_port(&self) -> String {
        DEFAULT_RUSTFS_API_PORT.to_string()
    }

    fn get_default_docker_image(&self) -> (String, String) {
        ("rustfs/rustfs".to_string(), "latest".to_string())
    }

    async fn get_current_docker_image(&self) -> Result<(String, String)> {
        let container_name = self.get_container_name();
        let info = self
            .docker
            .inspect_container(&container_name, None::<InspectContainerOptions>)
            .await?;

        if let Some(config) = info.config {
            if let Some(image) = config.image {
                if let Some((name, tag)) = image.split_once(':') {
                    return Ok((name.to_string(), tag.to_string()));
                }
                return Ok((image, "latest".to_string()));
            }
        }

        Err(anyhow::anyhow!("Could not determine current docker image"))
    }

    fn get_default_version(&self) -> String {
        "latest".to_string()
    }

    async fn get_current_version(&self) -> Result<String> {
        let (_, tag) = self.get_current_docker_image().await?;
        Ok(tag)
    }

    /// Backup RustFS data to another S3 location using mc mirror
    async fn backup_to_s3(
        &self,
        _s3_client: &aws_sdk_s3::Client,
        _s3_credentials: &super::S3Credentials,
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
            "Starting RustFS backup using MinIO Client for backup {}",
            backup.id
        );

        let backup_prefix = subpath_root;
        let container_name = format!("mc-backup-{}", backup.id);

        // Create a backup record
        let backup_record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
                service_id: Set(external_service.id),
                backup_id: Set(backup.id),
                backup_type: Set("full".to_string()),
                state: Set("running".to_string()),
                started_at: Set(Utc::now()),
                s3_location: Set(backup_prefix.to_string()),
                metadata: Set(serde_json::json!({
                    "service_type": "rustfs",
                    "service_name": self.name,
                    "timestamp": Utc::now().to_rfc3339(),
                })),
                compression_type: Set("none".to_string()),
                created_by: Set(0),
                ..Default::default()
            },
        )
        .exec_with_returning(pool)
        .await?;

        // Pull the MinIO Client image
        self.pull_mc_image(&self.docker).await?;

        let rustfs_config = self.get_rustfs_config(service_config)?;

        // Decrypt destination S3 credentials
        let dest_endpoint = s3_source
            .endpoint
            .clone()
            .unwrap_or(format!("{}:{}", s3_source.bucket_name, "9000"));
        let decrypted_access_key = self
            .encryption_service
            .decrypt_string(&s3_source.access_key_id)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt access key: {}", e))?;
        let decrypted_secret_key = self
            .encryption_service
            .decrypt_string(&s3_source.secret_key)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt secret key: {}", e))?;

        // Environment variables for mc - source is the RustFS service, dest is backup S3
        let env_vars = [
            format!(
                "MC_HOST_source=http://{}:{}@{}:{}",
                rustfs_config.access_key,
                rustfs_config.secret_key,
                rustfs_config.host,
                rustfs_config.port
            ),
            format!(
                "MC_HOST_dest=http://{}:{}@{}",
                decrypted_access_key, decrypted_secret_key, dest_endpoint
            ),
        ];

        // Create mc container with shell entrypoint and host networking
        let mc_config = bollard::models::ContainerCreateBody {
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

        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                mc_config,
            )
            .await?;

        self.docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        let source_endpoint = format!("http://{}:{}", rustfs_config.host, rustfs_config.port);
        let default_dest_endpoint = format!("http://{}:9000", s3_source.bucket_name);
        let dest_endpoint_str = s3_source
            .endpoint
            .as_deref()
            .unwrap_or(&default_dest_endpoint);
        let source_name = "original/".to_string();
        let dest_name = format!("backup-dest/{}/{}", s3_source.bucket_name, subpath_root);

        // Execute commands: set aliases then mirror
        let commands: Vec<Vec<&str>> = vec![
            vec![
                "mc",
                "alias",
                "set",
                "original",
                &source_endpoint,
                &rustfs_config.access_key,
                &rustfs_config.secret_key,
            ],
            vec![
                "mc",
                "alias",
                "set",
                "backup-dest",
                dest_endpoint_str,
                &decrypted_access_key,
                &decrypted_secret_key,
            ],
            vec!["mc", "mirror", "--overwrite", &source_name, &dest_name],
        ];

        let mut success = true;
        let mut error_logs = Vec::new();

        for cmd in commands {
            // Log only the subcommand — args may contain credentials (e.g. `mc alias set`).
            info!(
                "Executing command: {:?}",
                cmd.iter().take(3).collect::<Vec<_>>()
            );

            let (ok, _stdout, stderr) = self
                .exec_in_container(&self.docker, &container.id, cmd)
                .await?;

            if !ok {
                error_logs.push(stderr);
                success = false;
                break;
            }
        }

        // Clean up the mc container
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
            let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("completed".to_string());
            backup_update.finished_at = Set(Some(Utc::now()));
            temps_entities::external_service_backups::Entity::update(backup_update)
                .exec(pool)
                .await?;

            info!("RustFS backup completed successfully");
            Ok(backup_prefix.to_string())
        } else {
            let error_message = error_logs.join("\n");

            let mut backup_update: temps_entities::external_service_backups::ActiveModel =
                backup_record.clone().into();
            backup_update.state = Set("failed".to_string());
            backup_update.error_message = Set(Some(error_message.clone()));
            backup_update.finished_at = Set(Some(Utc::now()));
            temps_entities::external_service_backups::Entity::update(backup_update)
                .exec(pool)
                .await?;

            Err(anyhow::anyhow!("RustFS backup failed: {}", error_message))
        }
    }

    /// Restore RustFS data from an S3 backup using mc mirror
    async fn restore_from_s3(
        &self,
        _s3_client: &aws_sdk_s3::Client,
        _s3_credentials: &super::S3Credentials,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!(
            "Starting RustFS restore from backup location: {}",
            backup_location
        );

        // Ensure RustFS container is running before attempting restore
        self.start().await?;

        let docker = &self.docker;
        let container_name = format!("mc-restore-{}", uuid::Uuid::new_v4());
        let rustfs_config = self.get_rustfs_config(service_config)?;

        // Pull the MinIO Client image
        self.pull_mc_image(docker).await?;

        // s3_source credentials are expected to be plain-text (already decrypted by caller)
        let source_access_key = &s3_source.access_key_id;
        let source_secret_key = &s3_source.secret_key;
        let source_endpoint = s3_source.endpoint.as_deref().unwrap_or("s3.amazonaws.com");

        // Environment variables for mc - source is backup S3, dest is the RustFS service
        let env_vars = [
            format!(
                "MC_HOST_source=http://{}:{}@{}",
                source_access_key, source_secret_key, source_endpoint
            ),
            format!(
                "MC_HOST_dest=http://{}:{}@localhost:{}",
                rustfs_config.access_key, rustfs_config.secret_key, rustfs_config.port
            ),
        ];

        // Create mc container with shell entrypoint and host networking
        let mc_config = bollard::models::ContainerCreateBody {
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

        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                mc_config,
            )
            .await?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        let dest_endpoint = format!("http://localhost:{}", rustfs_config.port);

        // Set up aliases
        let setup_commands: Vec<Vec<&str>> = vec![
            vec![
                "mc",
                "alias",
                "set",
                "backup-source",
                source_endpoint,
                source_access_key,
                source_secret_key,
            ],
            vec![
                "mc",
                "alias",
                "set",
                "dest",
                &dest_endpoint,
                &rustfs_config.access_key,
                &rustfs_config.secret_key,
            ],
        ];

        for cmd in setup_commands {
            let (ok, _stdout, stderr) = self.exec_in_container(docker, &container.id, cmd).await?;
            if !ok {
                // Clean up on alias setup failure
                docker
                    .remove_container(
                        &container.id,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await?;
                return Err(anyhow::anyhow!(
                    "Failed to set up mc aliases for RustFS restore: {}",
                    stderr
                ));
            }
        }

        // List buckets in the backup location
        let source_backup_location = format!(
            "backup-source/{}/{}",
            s3_source.bucket_name, backup_location
        );
        let list_command = vec!["mc", "ls", "--json", &source_backup_location];

        let (_, list_stdout, _) = self
            .exec_in_container(docker, &container.id, list_command)
            .await?;

        // Parse bucket listing from JSON output
        let mut buckets = Vec::new();
        let json_objects = parse_multiline_json_output(&list_stdout)?;
        for listing in json_objects {
            if let (Some("folder"), Some(key)) = (
                listing.get("type").and_then(|t| t.as_str()),
                listing.get("key").and_then(|k| k.as_str()),
            ) {
                buckets.push(key.to_string());
            }
        }

        info!("Found buckets to restore: {:?}", buckets);

        // For each bucket, create it and mirror its contents
        for bucket in buckets {
            let bucket_name = bucket.trim_end_matches('/');
            let dest_location = format!("dest/{}", bucket_name);

            // Create bucket (ignore "already exists" errors)
            let create_bucket_cmd = vec!["mc", "mb", &dest_location];
            let (ok, stdout_mb, _) = self
                .exec_in_container(docker, &container.id, create_bucket_cmd)
                .await?;

            if !ok && !stdout_mb.contains("object name cannot be empty") {
                // Non-fatal: bucket may already exist, log and continue
                info!(
                    "Bucket creation returned non-zero for {}, continuing: {}",
                    bucket_name, stdout_mb
                );
            }

            // Mirror the bucket contents
            let source_bucket_loc = format!(
                "backup-source/{}/{}/{}",
                s3_source.bucket_name, backup_location, bucket_name
            );
            let dest_bucket_loc = format!("dest/{}", bucket_name);
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

            let (ok, _stdout, stderr) = self
                .exec_in_container(docker, &container.id, mirror_cmd)
                .await?;

            if !ok {
                error!("Mirror failed for bucket {}: {}", bucket_name, stderr);
                // Continue with other buckets rather than failing entirely
            }
        }

        // Clean up the mc container
        docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;

        info!("RustFS restore completed successfully");
        Ok(())
    }

    async fn restore_capabilities(
        &self,
        _service_config: ServiceConfig,
    ) -> Result<super::RestoreCapabilities> {
        Ok(super::RestoreCapabilities {
            restore_in_place: true,
            restore_to_new_service: true,
            // PITR for object stores would require versioned buckets + per-version
            // pointers; not yet wired up.
            pitr: false,
            earliest_pitr_time: None,
            latest_pitr_time: None,
        })
    }

    /// Provision a fresh RustFS service and mirror a backup into it.
    ///
    /// Strategy: clone the source service's config (image, region), generate new
    /// credentials and unused host ports, spin up a new container+volumes, then
    /// run `mc mirror` from the backup location into every bucket discovered
    /// under the backup prefix. The new service gets its OWN access keys.
    async fn restore_to_new_service(
        &self,
        ctx: super::RestoreContext<'_>,
        new_service_name: String,
        parameter_overrides: serde_json::Value,
    ) -> Result<super::NewServiceRestoreResult> {
        info!(
            "Provisioning new RustFS service '{}' from backup at {}",
            new_service_name, ctx.backup_location
        );

        // Start from the source service's parameters (image/region), then rewrite
        // port + credentials so we don't collide with the source.
        let mut new_config = self.get_rustfs_config(ctx.source_config.clone())?;

        let new_api_port = find_available_port(DEFAULT_RUSTFS_API_PORT)
            .ok_or_else(|| anyhow::anyhow!("No available API ports for new RustFS service"))?
            .to_string();
        let api_port_num: u16 = new_api_port.parse().unwrap_or(DEFAULT_RUSTFS_API_PORT);
        let console_start = std::cmp::max(api_port_num + 1, DEFAULT_RUSTFS_CONSOLE_PORT);
        let new_console_port = find_available_port(console_start)
            .ok_or_else(|| anyhow::anyhow!("No available console ports for new RustFS service"))?
            .to_string();

        new_config.port = new_api_port;
        new_config.console_port = new_console_port;
        new_config.access_key = default_access_key();
        new_config.secret_key = default_secret_key();

        if let Some(overrides) = parameter_overrides.as_object() {
            if let Some(port) = overrides.get("port").and_then(|v| v.as_str()) {
                new_config.port = port.to_string();
            }
            if let Some(port) = overrides.get("console_port").and_then(|v| v.as_str()) {
                new_config.console_port = port.to_string();
            }
            if let Some(image) = overrides.get("docker_image").and_then(|v| v.as_str()) {
                new_config.docker_image = image.to_string();
            }
            if let Some(ak) = overrides.get("access_key").and_then(|v| v.as_str()) {
                new_config.access_key = ak.to_string();
            }
            if let Some(sk) = overrides.get("secret_key").and_then(|v| v.as_str()) {
                new_config.secret_key = sk.to_string();
            }
        }

        let new_service = RustfsService::new(
            new_service_name.clone(),
            self.docker.clone(),
            self.encryption_service.clone(),
        );
        *new_service.config.write().await = Some(new_config.clone());

        new_service
            .create_container(&self.docker, &new_config)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create new RustFS container: {}", e))?;

        // Orchestrator already decrypted into the plaintext s3_source copy.
        // Re-decrypting would fail because these are no longer ciphertext.
        let source_access_key = ctx.s3_source.access_key_id.clone();
        let source_secret_key = ctx.s3_source.secret_key.clone();
        let source_endpoint = ctx
            .s3_source
            .endpoint
            .as_deref()
            .unwrap_or("s3.amazonaws.com");

        self.pull_mc_image(&self.docker).await?;

        let mc_container_name = format!("mc-restore-new-{}", uuid::Uuid::new_v4());
        let env_vars = [
            format!(
                "MC_HOST_source=http://{}:{}@{}",
                source_access_key, source_secret_key, source_endpoint
            ),
            format!(
                "MC_HOST_dest=http://{}:{}@localhost:{}",
                new_config.access_key, new_config.secret_key, new_config.port
            ),
        ];

        let mc_config = bollard::models::ContainerCreateBody {
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

        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&mc_container_name)
                        .build(),
                ),
                mc_config,
            )
            .await?;

        self.docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        let dest_endpoint = format!("http://localhost:{}", new_config.port);
        let setup_commands: Vec<Vec<&str>> = vec![
            vec![
                "mc",
                "alias",
                "set",
                "backup-source",
                source_endpoint,
                &source_access_key,
                &source_secret_key,
            ],
            vec![
                "mc",
                "alias",
                "set",
                "dest",
                &dest_endpoint,
                &new_config.access_key,
                &new_config.secret_key,
            ],
        ];

        for cmd in setup_commands {
            let (ok, _stdout, stderr) = self
                .exec_in_container(&self.docker, &container.id, cmd)
                .await?;
            if !ok {
                let _ = self
                    .docker
                    .remove_container(
                        &container.id,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                return Err(anyhow::anyhow!(
                    "Failed to set up mc aliases for new RustFS restore: {}",
                    stderr
                ));
            }
        }

        // List buckets at the backup prefix.
        let source_backup_location = format!(
            "backup-source/{}/{}",
            ctx.s3_source.bucket_name, ctx.backup_location
        );
        let list_command = vec!["mc", "ls", "--json", &source_backup_location];
        let (_, list_stdout, _) = self
            .exec_in_container(&self.docker, &container.id, list_command)
            .await?;

        let mut buckets: Vec<String> = Vec::new();
        let json_objects = parse_multiline_json_output(&list_stdout)?;
        for listing in json_objects {
            if let (Some("folder"), Some(key)) = (
                listing.get("type").and_then(|t| t.as_str()),
                listing.get("key").and_then(|k| k.as_str()),
            ) {
                buckets.push(key.to_string());
            }
        }

        info!(
            "Restoring {} bucket(s) into new RustFS service '{}'",
            buckets.len(),
            new_service_name
        );

        for bucket in buckets {
            let bucket_name = bucket.trim_end_matches('/');
            let dest_location = format!("dest/{}", bucket_name);

            let mb_cmd = vec!["mc", "mb", &dest_location];
            let (ok, stdout_mb, _) = self
                .exec_in_container(&self.docker, &container.id, mb_cmd)
                .await?;
            if !ok && !stdout_mb.contains("already") {
                info!(
                    "mc mb returned non-zero for bucket {}, continuing: {}",
                    bucket_name, stdout_mb
                );
            }

            let source_bucket_loc = format!(
                "backup-source/{}/{}/{}",
                ctx.s3_source.bucket_name, ctx.backup_location, bucket_name
            );
            let mirror_cmd = vec![
                "mc",
                "mirror",
                "--skip-errors",
                "--overwrite",
                &source_bucket_loc,
                &dest_location,
            ];

            info!("Mirroring bucket {} -> new RustFS service", bucket_name);
            let (ok, _stdout, stderr) = self
                .exec_in_container(&self.docker, &container.id, mirror_cmd)
                .await?;
            if !ok {
                error!("Mirror failed for bucket {}: {}", bucket_name, stderr);
            }
        }

        let _ = self
            .docker
            .remove_container(
                &container.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let runtime_json = serde_json::to_value(&new_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize runtime config: {}", e))?;
        let mut parameters = HashMap::new();
        if let Some(obj) = runtime_json.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    parameters.insert(k.clone(), s.to_string());
                } else if let Some(n) = v.as_u64() {
                    parameters.insert(k.clone(), n.to_string());
                }
            }
        }

        let connection_info = format!(
            "s3://{}:***@{}:{}",
            new_config.access_key, new_config.host, new_config.port
        );

        Ok(super::NewServiceRestoreResult {
            parameters,
            connection_info,
        })
    }
}

/// Parse multiline JSON output from `mc ls --json` (one JSON object per line)
fn parse_multiline_json_output(output: &str) -> Result<Vec<serde_json::Value>> {
    let mut json_objects = Vec::new();
    let mut current_object = String::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        current_object.push_str(trimmed);

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
    fn test_rustfs_config_defaults() {
        let input = RustfsInputConfig {
            port: None,
            console_port: None,
            access_key: None,
            secret_key: None,
            host: default_host(),
            region: default_region(),
            docker_image: default_image(),
        };

        let config = RustfsConfig::from(input);

        assert_eq!(config.host, "localhost");
        assert_eq!(config.region, "us-east-1");
        assert_eq!(config.docker_image, DEFAULT_RUSTFS_IMAGE);
        assert!(!config.access_key.is_empty());
        assert!(!config.secret_key.is_empty());
    }

    #[test]
    fn test_rustfs_config_custom() {
        let input = RustfsInputConfig {
            port: Some("9100".to_string()),
            console_port: Some("9101".to_string()),
            access_key: Some("myaccesskey".to_string()),
            secret_key: Some("mysecretkey".to_string()),
            host: "custom-host".to_string(),
            region: "eu-west-1".to_string(),
            docker_image: "rustfs/rustfs:1.0.0".to_string(),
        };

        let config = RustfsConfig::from(input);

        assert_eq!(config.port, "9100");
        assert_eq!(config.console_port, "9101");
        assert_eq!(config.access_key, "myaccesskey");
        assert_eq!(config.secret_key, "mysecretkey");
        assert_eq!(config.host, "custom-host");
        assert_eq!(config.region, "eu-west-1");
        assert_eq!(config.docker_image, "rustfs/rustfs:1.0.0");
    }

    #[test]
    fn test_access_key_format() {
        let key = default_access_key();
        assert!(key.starts_with("AKIA"));
        assert_eq!(key.len(), 20);
    }

    #[test]
    fn test_secret_key_format() {
        let key = default_secret_key();
        assert_eq!(key.len(), 40);
    }
}
