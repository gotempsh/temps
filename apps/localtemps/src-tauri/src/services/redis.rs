//! Minimal Redis service for LocalTemps
//!
//! Manages a Redis Docker container for local KV storage.

use anyhow::Result;
use bollard::models::{ContainerCreateBody, ContainerSummaryStateEnum};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptions, RemoveContainerOptions,
    StopContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;
use redis::aio::ConnectionManager;
use redis::Client;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info};

/// Default Redis Docker image
pub const DEFAULT_REDIS_IMAGE: &str = "redis:8-alpine";
/// Default Redis port
const DEFAULT_REDIS_PORT: u16 = 6379;
/// Fixed password for LocalTemps Redis (allows container reuse)
const DEFAULT_REDIS_PASSWORD: &str = "localtemps-redis-secret";

/// Redis configuration
#[derive(Debug, Clone)]
pub struct RedisConfig {
    pub host: String,
    pub port: u16,
    pub password: String,
    pub docker_image: String,
    pub container_name: String,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: DEFAULT_REDIS_PORT,
            password: DEFAULT_REDIS_PASSWORD.to_string(),
            docker_image: DEFAULT_REDIS_IMAGE.to_string(),
            container_name: "localtemps-redis".to_string(),
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

fn find_available_port(start_port: u16) -> Option<u16> {
    (start_port..start_port + 100).find(|&port| is_port_available(port))
}

/// Minimal Redis service for LocalTemps
pub struct RedisService {
    docker: Arc<Docker>,
    config: Arc<RwLock<Option<RedisConfig>>>,
}

impl RedisService {
    pub fn new(docker: Arc<Docker>) -> Self {
        Self {
            docker,
            config: Arc::new(RwLock::new(None)),
        }
    }

    /// Initialize and start the Redis container
    pub async fn init(&self) -> Result<()> {
        // Check if container already exists and is running
        if let Some(config) = self.try_reuse_existing_container().await? {
            info!("Reusing existing Redis container on port {}", config.port);
            *self.config.write().await = Some(config);
            return Ok(());
        }

        // Find an available port
        let port = find_available_port(DEFAULT_REDIS_PORT)
            .ok_or_else(|| anyhow::anyhow!("No available port found for Redis"))?;

        let config = RedisConfig {
            port,
            ..Default::default()
        };

        info!("Starting Redis container on port {}", port);

        // Pull the image
        self.pull_image(&config.docker_image).await?;

        // Create and start the container
        self.create_container(&config).await?;

        // Wait for Redis to be ready
        self.wait_for_ready(&config).await?;

        // Store the config
        *self.config.write().await = Some(config);

        info!("Redis service initialized successfully");
        Ok(())
    }

    /// Try to reuse an existing container (running or stopped)
    async fn try_reuse_existing_container(&self) -> Result<Option<RedisConfig>> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true, // Include stopped containers
                ..Default::default()
            }))
            .await?;

        let container_name = RedisConfig::default().container_name;

        for container in containers {
            if let Some(names) = &container.names {
                if names.iter().any(|n| n.contains(&container_name)) {
                    let is_running = container.state == Some(ContainerSummaryStateEnum::RUNNING);

                    // If container is stopped, start it first
                    if !is_running {
                        info!("Starting stopped Redis container {}", container_name);
                        self.docker
                            .start_container(
                                &container_name,
                                None::<bollard::query_parameters::StartContainerOptions>,
                            )
                            .await?;
                        // Wait a bit for container to start
                        sleep(Duration::from_secs(1)).await;
                    }

                    // Extract port from port bindings
                    if let Some(ports) = &container.ports {
                        for port in ports {
                            if port.private_port == 6379 {
                                if let Some(public_port) = port.public_port {
                                    let config = RedisConfig {
                                        port: public_port,
                                        ..Default::default()
                                    };

                                    // Try to connect with our fixed password
                                    match self.try_connect(&config).await {
                                        Ok(_) => {
                                            info!(
                                                "Reusing existing Redis container on port {}",
                                                public_port
                                            );
                                            return Ok(Some(config));
                                        }
                                        Err(e) => {
                                            // Container exists but connection failed - likely old random password
                                            // Return error asking user to remove the old container
                                            return Err(anyhow::anyhow!(
                                                "Found existing Redis container '{}' on port {} but cannot connect ({}). \
                                                This container may have been created with a different password. \
                                                Please remove it manually: docker rm -f {}",
                                                container_name, public_port, e, container_name
                                            ));
                                        }
                                    }
                                }
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

        info!("Pulling Redis image {}:{}", image_name, tag);

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
                    return Err(anyhow::anyhow!("Failed to pull Redis image: {}", e));
                }
            }
        }

        Ok(())
    }

    async fn create_container(&self, config: &RedisConfig) -> Result<()> {
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
        info!("Creating new Redis container {}", config.container_name);

        // Create port bindings
        let mut port_bindings = HashMap::new();
        port_bindings.insert(
            "6379/tcp".to_string(),
            Some(vec![bollard::models::PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(config.port.to_string()),
            }]),
        );

        // Create container config using the new API
        let container_config = ContainerCreateBody {
            image: Some(config.docker_image.clone()),
            cmd: Some(vec![
                "redis-server".to_string(),
                "--requirepass".to_string(),
                config.password.clone(),
            ]),
            host_config: Some(bollard::models::HostConfig {
                port_bindings: Some(port_bindings),
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

    async fn wait_for_ready(&self, config: &RedisConfig) -> Result<()> {
        let max_retries = 30;
        let retry_delay = Duration::from_millis(500);

        for i in 0..max_retries {
            match self.try_connect(config).await {
                Ok(_) => {
                    info!("Redis is ready after {} attempts", i + 1);
                    return Ok(());
                }
                Err(e) => {
                    if i == max_retries - 1 {
                        return Err(anyhow::anyhow!(
                            "Redis failed to become ready after {} attempts: {}",
                            max_retries,
                            e
                        ));
                    }
                    sleep(retry_delay).await;
                }
            }
        }

        Ok(())
    }

    async fn try_connect(&self, config: &RedisConfig) -> Result<()> {
        let url = format!(
            "redis://:{}@localhost:{}",
            urlencoding::encode(&config.password),
            config.port
        );
        let client = Client::open(url.as_str())?;
        let mut conn = client.get_multiplexed_tokio_connection().await?;
        redis::cmd("PING").query_async::<String>(&mut conn).await?;
        Ok(())
    }

    /// Get a Redis connection
    pub async fn get_connection(&self) -> Result<ConnectionManager> {
        let config = self
            .config
            .read()
            .await
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Redis not initialized"))?
            .clone();

        let url = format!(
            "redis://:{}@localhost:{}",
            urlencoding::encode(&config.password),
            config.port
        );

        let client = Client::open(url.as_str())?;
        let conn = tokio::time::timeout(Duration::from_secs(5), ConnectionManager::new(client))
            .await
            .map_err(|_| anyhow::anyhow!("Redis connection timed out"))??;

        Ok(conn)
    }

    /// Check if Redis is healthy
    pub async fn health_check(&self) -> Result<bool> {
        let config = match self.config.read().await.as_ref() {
            Some(c) => c.clone(),
            None => return Ok(false),
        };

        match self.try_connect(&config).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Get connection info
    pub fn get_connection_info(&self) -> Result<String> {
        let config = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Config locked"))?;

        match config.as_ref() {
            Some(c) => Ok(format!("redis://localhost:{}", c.port)),
            None => Err(anyhow::anyhow!("Redis not initialized")),
        }
    }

    /// Stop the Redis container
    pub async fn stop(&self) -> Result<()> {
        let config = match self.config.read().await.as_ref() {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        info!("Stopping Redis container {}", config.container_name);

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

    /// Remove the Redis container
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

        Ok(())
    }
}
