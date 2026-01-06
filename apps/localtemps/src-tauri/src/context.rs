//! LocalTemps Context - wraps Temps services for local development
//!
//! This module provides a simplified context that wraps KvService and BlobService
//! without requiring database dependencies. Perfect for local development workflows.

use std::sync::Arc;

use anyhow::Result;
use bollard::Docker;
use serde_json::json;
use temps_blob::services::BlobService;
use temps_core::EncryptionService;
use temps_kv::services::KvService;
use temps_providers::externalsvc::{
    ExternalService, RedisService, RustfsService, ServiceConfig, ServiceType,
};
use tokio::sync::RwLock;
use tracing::{error, info};

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
    #[allow(dead_code)]
    encryption_service: Arc<EncryptionService>,

    // External services (Docker containers)
    redis_service: Arc<RedisService>,
    rustfs_service: Arc<RustfsService>,

    // Business logic services
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

        // Create encryption service with a fixed local key
        // This key is only used locally, so security is not a concern
        let local_encryption_key =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let encryption_service = Arc::new(
            EncryptionService::new(local_encryption_key)
                .map_err(|e| anyhow::anyhow!("Failed to create encryption service: {}", e))?,
        );

        // Create Redis service for KV
        let redis_service = Arc::new(RedisService::new(
            "localtemps-kv".to_string(),
            docker.clone(),
        ));

        // Create RustFS service for Blob
        let rustfs_service = Arc::new(RustfsService::new(
            "localtemps-blob".to_string(),
            docker.clone(),
            encryption_service.clone(),
        ));

        // Create business logic services
        let kv_service = Arc::new(KvService::new(redis_service.clone()));
        let blob_service = Arc::new(BlobService::new(rustfs_service.clone()));

        Ok(Self {
            docker,
            encryption_service,
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
        let redis_config = ServiceConfig {
            name: "localtemps-kv".to_string(),
            service_type: ServiceType::Kv,
            version: None,
            parameters: json!({
                "port": "6379"
            }),
        };

        self.redis_service
            .init(redis_config)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to initialize Redis: {}", e))?;

        info!("Redis container started");

        // Initialize RustFS service
        info!("Starting RustFS container...");
        let rustfs_config = ServiceConfig {
            name: "localtemps-blob".to_string(),
            service_type: ServiceType::Blob,
            version: None,
            parameters: json!({
                "port": "9000",
                "console_port": "9001",
                "host": "localhost"
            }),
        };

        self.rustfs_service
            .init(rustfs_config)
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
        let redis_port = if redis_running { Some(6379) } else { None };
        statuses.push(ServiceStatus {
            name: "Redis (KV)".to_string(),
            service_type: "kv".to_string(),
            running: redis_running,
            port: redis_port,
            connection_info: if redis_running {
                self.redis_service.get_connection_info().ok()
            } else {
                None
            },
        });

        // RustFS status
        let rustfs_running = self.rustfs_service.health_check().await.unwrap_or(false);
        let rustfs_port = if rustfs_running { Some(9000) } else { None };
        statuses.push(ServiceStatus {
            name: "RustFS (Blob)".to_string(),
            service_type: "blob".to_string(),
            running: rustfs_running,
            port: rustfs_port,
            connection_info: if rustfs_running {
                self.rustfs_service.get_connection_info().ok()
            } else {
                None
            },
        });

        statuses
    }

    /// Check if services are initialized
    pub async fn is_initialized(&self) -> bool {
        *self.services_initialized.read().await
    }

    /// Ensure services are initialized (auto-start if needed)
    ///
    /// This is called automatically by API handlers to provide a zero-config
    /// developer experience. Services start on first API call.
    pub async fn ensure_initialized(&self) -> Result<()> {
        // Quick check without write lock
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

        // Initialize services
        self.init_services().await
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
