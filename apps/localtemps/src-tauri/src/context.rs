//! LocalTemps Context - standalone local development environment
//!
//! This module provides a simplified context that manages Redis and RustFS
//! containers for local development without requiring any temps-* crate dependencies.

use std::sync::Arc;

use anyhow::Result;
use bollard::Docker;
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::services::{BlobService, KvService, RedisService, RustfsService};

/// Fixed local development token
pub const LOCAL_TOKEN: &str = "localtemps-dev-token";

/// Fixed local project ID
pub const LOCAL_PROJECT_ID: i32 = 1;

/// Default API port
pub const DEFAULT_API_PORT: u16 = 4000;

/// Service status
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub service_type: String,
    pub running: bool,
    pub port: Option<u16>,
    pub connection_info: Option<String>,
}

/// LocalTemps context containing all services
pub struct LocalTempsContext {
    #[allow(dead_code)]
    docker: Arc<Docker>,

    // Infrastructure services
    redis_service: Arc<RedisService>,
    rustfs_service: Arc<RustfsService>,

    // Application services
    kv_service: Arc<KvService>,
    blob_service: Arc<BlobService>,

    // Service state
    services_initialized: Arc<RwLock<bool>>,
}

impl LocalTempsContext {
    /// Create a new LocalTemps context
    pub async fn new() -> Result<Self> {
        info!("Creating LocalTemps context...");

        // Connect to Docker
        let docker = Arc::new(
            Docker::connect_with_local_defaults()
                .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?,
        );

        info!("Connected to Docker");

        // Create infrastructure services
        let redis_service = Arc::new(RedisService::new(docker.clone()));
        let rustfs_service = Arc::new(RustfsService::new(docker.clone()));

        // Create application services
        let kv_service = Arc::new(KvService::new(redis_service.clone()));
        let blob_service = Arc::new(BlobService::new(rustfs_service.clone()));

        Ok(Self {
            docker,
            redis_service,
            rustfs_service,
            kv_service,
            blob_service,
            services_initialized: Arc::new(RwLock::new(false)),
        })
    }

    /// Initialize all services (start Docker containers)
    pub async fn init_services(&self) -> Result<()> {
        let mut initialized = self.services_initialized.write().await;
        if *initialized {
            info!("Services already initialized");
            return Ok(());
        }

        info!("Initializing LocalTemps services...");

        // Initialize Redis service
        info!("Starting Redis container...");
        self.redis_service
            .init()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to initialize Redis: {}", e))?;
        info!("Redis container started");

        // Initialize RustFS service
        info!("Starting RustFS container...");
        self.rustfs_service
            .init()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to initialize RustFS: {}", e))?;
        info!("RustFS container started");

        *initialized = true;
        info!("All LocalTemps services initialized successfully");

        Ok(())
    }

    /// Stop all services
    pub async fn stop_services(&self) -> Result<()> {
        info!("Stopping LocalTemps services...");

        // Stop Redis
        if let Err(e) = self.redis_service.stop().await {
            error!("Failed to stop Redis: {}", e);
        }

        // Stop RustFS
        if let Err(e) = self.rustfs_service.stop().await {
            error!("Failed to stop RustFS: {}", e);
        }

        let mut initialized = self.services_initialized.write().await;
        *initialized = false;

        info!("All LocalTemps services stopped");
        Ok(())
    }

    /// Get service status
    pub async fn get_service_status(&self) -> Vec<ServiceStatus> {
        let mut statuses = Vec::new();

        // Redis status
        let redis_running = self.redis_service.health_check().await.unwrap_or(false);
        let redis_connection_info = if redis_running {
            self.redis_service.get_connection_info().ok()
        } else {
            None
        };
        // Extract port from connection info (e.g., "redis://localhost:6379" -> 6379)
        let redis_port = redis_connection_info
            .as_ref()
            .and_then(|info| info.rsplit(':').next())
            .and_then(|port_str| port_str.parse::<u16>().ok());
        statuses.push(ServiceStatus {
            name: "Redis (KV)".to_string(),
            service_type: "kv".to_string(),
            running: redis_running,
            port: redis_port,
            connection_info: redis_connection_info,
        });

        // RustFS status
        let rustfs_running = self.rustfs_service.health_check().await.unwrap_or(false);
        let rustfs_connection_info = if rustfs_running {
            self.rustfs_service.get_connection_info().ok()
        } else {
            None
        };
        // Extract port from connection info (e.g., "http://localhost:9000" -> 9000)
        let rustfs_port = rustfs_connection_info
            .as_ref()
            .and_then(|info| info.rsplit(':').next())
            .and_then(|port_str| port_str.parse::<u16>().ok());
        statuses.push(ServiceStatus {
            name: "RustFS (Blob)".to_string(),
            service_type: "blob".to_string(),
            running: rustfs_running,
            port: rustfs_port,
            connection_info: rustfs_connection_info,
        });

        statuses
    }

    /// Check if services are initialized
    pub async fn is_initialized(&self) -> bool {
        *self.services_initialized.read().await
    }

    /// Ensure services are initialized (auto-start if needed)
    pub async fn ensure_initialized(&self) -> Result<()> {
        if self.is_initialized().await {
            // Double-check health
            let redis_healthy = self.redis_service.health_check().await.unwrap_or(false);
            let rustfs_healthy = self.rustfs_service.health_check().await.unwrap_or(false);

            if redis_healthy && rustfs_healthy {
                return Ok(());
            }

            // Services not healthy, need to reinitialize
            info!("Services not healthy, reinitializing...");
            let mut initialized = self.services_initialized.write().await;
            *initialized = false;
        }

        self.init_services().await
    }

    /// Get Redis service reference
    pub fn redis_service(&self) -> Arc<RedisService> {
        self.redis_service.clone()
    }

    /// Get RustFS service reference
    pub fn rustfs_service(&self) -> Arc<RustfsService> {
        self.rustfs_service.clone()
    }

    /// Get KV service reference
    pub fn kv_service(&self) -> Arc<KvService> {
        self.kv_service.clone()
    }

    /// Get Blob service reference
    pub fn blob_service(&self) -> Arc<BlobService> {
        self.blob_service.clone()
    }
}
