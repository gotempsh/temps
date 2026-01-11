//! Minimal RustFS service for LocalTemps
//!
//! Manages a RustFS Docker container for local blob storage.

use anyhow::Result;
use aws_sdk_s3::Client;
use bollard::models::{ContainerCreateBody, ContainerSummaryStateEnum};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptions, RemoveContainerOptions,
    StopContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info};

/// Default RustFS Docker image (from Docker Hub)
pub const DEFAULT_RUSTFS_IMAGE: &str = "rustfs/rustfs:latest";
/// Default RustFS API port
const DEFAULT_RUSTFS_API_PORT: u16 = 9000;
/// Default RustFS console port
const DEFAULT_RUSTFS_CONSOLE_PORT: u16 = 9001;
/// Default access key
const DEFAULT_ACCESS_KEY: &str = "localtemps";
/// Default secret key
const DEFAULT_SECRET_KEY: &str = "localtemps123456";

/// RustFS configuration
#[derive(Debug, Clone)]
pub struct RustfsConfig {
    pub host: String,
    pub port: u16,
    pub console_port: u16,
    pub access_key: String,
    pub secret_key: String,
    pub docker_image: String,
    pub container_name: String,
}

impl Default for RustfsConfig {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: DEFAULT_RUSTFS_API_PORT,
            console_port: DEFAULT_RUSTFS_CONSOLE_PORT,
            access_key: DEFAULT_ACCESS_KEY.to_string(),
            secret_key: DEFAULT_SECRET_KEY.to_string(),
            docker_image: DEFAULT_RUSTFS_IMAGE.to_string(),
            container_name: "localtemps-rustfs".to_string(),
        }
    }
}

fn is_port_available(port: u16) -> bool {
    // Check if we can bind to the port (not in use by host processes)
    if TcpListener::bind(("0.0.0.0", port)).is_err() {
        return false;
    }
    // Also check localhost binding
    if TcpListener::bind(("127.0.0.1", port)).is_err() {
        return false;
    }
    true
}

fn find_available_port(start_port: u16, skip_ports: &[u16]) -> Option<u16> {
    (start_port..start_port + 100)
        .filter(|port| !skip_ports.contains(port))
        .find(|&port| is_port_available(port))
}

/// Minimal RustFS service for LocalTemps
pub struct RustfsService {
    docker: Arc<Docker>,
    config: Arc<RwLock<Option<RustfsConfig>>>,
    client: Arc<RwLock<Option<Client>>>,
}

impl RustfsService {
    pub fn new(docker: Arc<Docker>) -> Self {
        Self {
            docker,
            config: Arc::new(RwLock::new(None)),
            client: Arc::new(RwLock::new(None)),
        }
    }

    /// Initialize and start the RustFS container
    pub async fn init(&self) -> Result<()> {
        // Check if container already exists and is running
        if let Some(config) = self.try_reuse_existing_container().await? {
            info!("Reusing existing RustFS container on port {}", config.port);
            // Create S3 client for existing container
            let client = self.create_s3_client(&config).await?;
            *self.client.write().await = Some(client);
            *self.config.write().await = Some(config);
            return Ok(());
        }

        // Find available ports - ensure they don't overlap
        let api_port = find_available_port(DEFAULT_RUSTFS_API_PORT, &[])
            .ok_or_else(|| anyhow::anyhow!("No available port found for RustFS API"))?;
        // Start console port search after api_port to avoid potential overlap
        let console_port =
            find_available_port(DEFAULT_RUSTFS_CONSOLE_PORT.max(api_port + 1), &[api_port])
                .ok_or_else(|| anyhow::anyhow!("No available port found for RustFS console"))?;

        let config = RustfsConfig {
            port: api_port,
            console_port,
            ..Default::default()
        };

        info!(
            "Starting RustFS container on ports {} (API) and {} (console)",
            api_port, console_port
        );

        // Pull the image
        self.pull_image(&config.docker_image).await?;

        // Create and start the container
        self.create_container(&config).await?;

        // Wait for RustFS to be ready
        self.wait_for_ready(&config).await?;

        // Create S3 client
        let client = self.create_s3_client(&config).await?;
        *self.client.write().await = Some(client);

        // Store the config
        *self.config.write().await = Some(config);

        info!("RustFS service initialized successfully");
        Ok(())
    }

    /// Try to reuse an existing container (running or stopped)
    async fn try_reuse_existing_container(&self) -> Result<Option<RustfsConfig>> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true, // Include stopped containers
                ..Default::default()
            }))
            .await?;

        let container_name = RustfsConfig::default().container_name;

        for container in containers {
            if let Some(names) = &container.names {
                if names.iter().any(|n| n.contains(&container_name)) {
                    let is_running = container.state == Some(ContainerSummaryStateEnum::RUNNING);

                    // If container is stopped, start it first
                    if !is_running {
                        info!("Starting stopped RustFS container {}", container_name);
                        self.docker
                            .start_container(
                                &container_name,
                                None::<bollard::query_parameters::StartContainerOptions>,
                            )
                            .await?;
                        // Wait a bit for container to start
                        sleep(Duration::from_secs(2)).await;
                    }

                    // Extract ports from port bindings
                    if let Some(ports) = &container.ports {
                        let mut api_port: Option<u16> = None;
                        let mut console_port: Option<u16> = None;

                        for port in ports {
                            if port.private_port == 9000 {
                                api_port = port.public_port;
                            } else if port.private_port == 9001 {
                                console_port = port.public_port;
                            }
                        }

                        if let (Some(api), Some(console)) = (api_port, console_port) {
                            let config = RustfsConfig {
                                port: api,
                                console_port: console,
                                ..Default::default()
                            };

                            // Verify we can connect via health check
                            if self.check_health(&config).await.unwrap_or(false) {
                                info!("Reusing existing RustFS container on port {}", api);
                                return Ok(Some(config));
                            } else {
                                // Container exists but health check failed
                                // Wait a bit more and try again
                                sleep(Duration::from_secs(2)).await;
                                if self.check_health(&config).await.unwrap_or(false) {
                                    info!(
                                        "Reusing existing RustFS container on port {} (after retry)",
                                        api
                                    );
                                    return Ok(Some(config));
                                }
                                // Still failing - return error
                                return Err(anyhow::anyhow!(
                                    "Found existing RustFS container '{}' on port {} but health check failed. \
                                    Please check container logs or remove it manually: docker rm -f {}",
                                    container_name, api, container_name
                                ));
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    async fn pull_image(&self, image: &str) -> Result<()> {
        let (image_name, tag) = if let Some((name, tag)) = image.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (image.to_string(), "latest".to_string())
        };

        info!("Pulling RustFS image {}:{}", image_name, tag);

        let mut stream = self.docker.create_image(
            Some(bollard::query_parameters::CreateImageOptions {
                from_image: Some(image_name),
                tag: Some(tag),
                ..Default::default()
            }),
            None,
            None,
        );

        while let Some(result) = stream.next().await {
            match result {
                Ok(_) => {}
                Err(e) => {
                    error!("Error pulling image: {}", e);
                    return Err(anyhow::anyhow!("Failed to pull RustFS image: {}", e));
                }
            }
        }

        Ok(())
    }

    async fn create_container(&self, config: &RustfsConfig) -> Result<()> {
        // Check if container already exists - if so, just start it (don't remove!)
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                ..Default::default()
            }))
            .await?;

        for container in containers {
            if let Some(names) = &container.names {
                if names.iter().any(|n| n.contains(&config.container_name)) {
                    // Container exists - check if it's running
                    if container.state == Some(ContainerSummaryStateEnum::RUNNING) {
                        info!(
                            "Container {} is already running, skipping creation",
                            config.container_name
                        );
                        return Ok(());
                    }
                    // Container exists but not running - try to start it
                    info!(
                        "Container {} exists but not running, starting it",
                        config.container_name
                    );
                    self.docker
                        .start_container(
                            &config.container_name,
                            None::<bollard::query_parameters::StartContainerOptions>,
                        )
                        .await?;
                    return Ok(());
                }
            }
        }

        // No existing container - create a new one
        info!("Creating new RustFS container {}", config.container_name);

        // Create port bindings
        let mut port_bindings = HashMap::new();
        port_bindings.insert(
            "9000/tcp".to_string(),
            Some(vec![bollard::models::PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(config.port.to_string()),
            }]),
        );
        port_bindings.insert(
            "9001/tcp".to_string(),
            Some(vec![bollard::models::PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(config.console_port.to_string()),
            }]),
        );

        // Create environment variables
        let env = vec![
            format!("RUSTFS_ROOT_USER={}", config.access_key),
            format!("RUSTFS_ROOT_PASSWORD={}", config.secret_key),
            // Set access and secret keys explicitly
            format!("RUSTFS_ACCESS_KEY={}", config.access_key),
            format!("RUSTFS_SECRET_KEY={}", config.secret_key),
        ];

        // Create a named volume for data persistence
        let volume_name = format!("{}-data", config.container_name);

        // Try to create the volume (ignore if already exists)
        let _ = self
            .docker
            .create_volume(bollard::models::VolumeCreateOptions {
                name: Some(volume_name.clone()),
                ..Default::default()
            })
            .await;

        // Create container config using the new API
        let container_config = ContainerCreateBody {
            image: Some(config.docker_image.clone()),
            env: Some(env),
            cmd: Some(vec!["/data".to_string()]),
            host_config: Some(bollard::models::HostConfig {
                port_bindings: Some(port_bindings),
                binds: Some(vec![format!("{}:/data", volume_name)]),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Create the container
        self.docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&config.container_name)
                        .build(),
                ),
                container_config,
            )
            .await?;

        // Start the container
        self.docker
            .start_container(
                &config.container_name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await?;

        Ok(())
    }

    async fn wait_for_ready(&self, config: &RustfsConfig) -> Result<()> {
        let max_retries = 30;
        let retry_delay = Duration::from_millis(500);

        for i in 0..max_retries {
            match self.check_health(config).await {
                Ok(true) => {
                    info!("RustFS is ready after {} attempts", i + 1);
                    return Ok(());
                }
                _ => {
                    if i == max_retries - 1 {
                        return Err(anyhow::anyhow!(
                            "RustFS failed to become ready after {} attempts",
                            max_retries
                        ));
                    }
                    sleep(retry_delay).await;
                }
            }
        }

        Ok(())
    }

    async fn check_health(&self, config: &RustfsConfig) -> Result<bool> {
        let url = format!("http://localhost:{}/health", config.port);
        let client = reqwest::Client::new();
        match client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    async fn create_s3_client(&self, config: &RustfsConfig) -> Result<Client> {
        let endpoint = format!("http://localhost:{}", config.port);

        let creds = aws_sdk_s3::config::Credentials::new(
            &config.access_key,
            &config.secret_key,
            None,
            None,
            "localtemps",
        );

        let s3_config = aws_sdk_s3::Config::builder()
            .endpoint_url(endpoint)
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(creds)
            .force_path_style(true)
            .behavior_version_latest()
            .build();

        Ok(Client::from_conf(s3_config))
    }

    /// Get the S3 client
    pub async fn get_client(&self) -> Result<Client> {
        let client = self
            .client
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("RustFS not initialized"))?
            .clone();
        Ok(client)
    }

    /// Check if RustFS is healthy
    pub async fn health_check(&self) -> Result<bool> {
        let config = match self.config.read().await.as_ref() {
            Some(c) => c.clone(),
            None => return Ok(false),
        };

        self.check_health(&config).await
    }

    /// Get connection info
    pub fn get_connection_info(&self) -> Result<String> {
        let config = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Config locked"))?;

        match config.as_ref() {
            Some(c) => Ok(format!("http://localhost:{}", c.port)),
            None => Err(anyhow::anyhow!("RustFS not initialized")),
        }
    }

    /// Stop the RustFS container
    pub async fn stop(&self) -> Result<()> {
        let config = match self.config.read().await.as_ref() {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        info!("Stopping RustFS container {}", config.container_name);

        let _ = self
            .docker
            .stop_container(
                &config.container_name,
                Some(StopContainerOptions {
                    t: Some(5),
                    signal: None,
                }),
            )
            .await;

        Ok(())
    }

    /// Remove the RustFS container
    pub async fn remove(&self) -> Result<()> {
        let config = match self.config.read().await.as_ref() {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        self.docker
            .remove_container(
                &config.container_name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;

        *self.config.write().await = None;
        *self.client.write().await = None;

        Ok(())
    }
}
