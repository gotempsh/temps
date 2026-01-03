use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

pub mod mongodb;
pub mod postgres;
pub mod redis;
pub mod rustfs;
pub mod s3;

// Test utilities for backup and restore testing
#[cfg(test)]
pub mod test_utils;

// Re-export services for easier access
pub use mongodb::MongodbService;
pub use postgres::PostgresService;
pub use redis::RedisService;
pub use rustfs::RustfsService;
pub use s3::S3Service;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub parameters: serde_json::Value,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Mongodb,
    Postgres,
    Redis,
    S3,
    /// Temps KV service (Redis-backed key-value store)
    Kv,
    /// Temps Blob service (MinIO-backed object storage)
    Blob,
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceType::Mongodb => write!(f, "mongodb"),
            ServiceType::Postgres => write!(f, "postgres"),
            ServiceType::Redis => write!(f, "redis"),
            ServiceType::S3 => write!(f, "s3"),
            ServiceType::Kv => write!(f, "kv"),
            ServiceType::Blob => write!(f, "blob"),
        }
    }
}

impl ServiceType {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "mongodb" => Ok(ServiceType::Mongodb),
            "postgres" => Ok(ServiceType::Postgres),
            "redis" => Ok(ServiceType::Redis),
            "s3" => Ok(ServiceType::S3),
            "kv" => Ok(ServiceType::Kv),
            "blob" => Ok(ServiceType::Blob),
            _ => Err(anyhow::anyhow!("Invalid service type: {}", s)),
        }
    }

    /// Returns a Vec containing all available service types
    pub fn get_all() -> Vec<ServiceType> {
        vec![
            ServiceType::Mongodb,
            ServiceType::Postgres,
            ServiceType::Redis,
            ServiceType::S3,
            ServiceType::Kv,
            ServiceType::Blob,
        ]
    }

    /// Returns a Vec containing string representations of all available service types
    pub fn get_all_strings() -> Vec<String> {
        Self::get_all()
            .into_iter()
            .map(|st| st.to_string())
            .collect()
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceParameter {
    pub name: String,
    pub required: bool,
    pub encrypted: bool,
    pub description: String,
    pub default_value: Option<String>,
    pub validation_pattern: Option<String>,
    /// Optional list of valid choices for this parameter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalResource {
    pub name: String,
    pub resource_type: String,
    pub credentials: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEnvVar {
    pub name: String,
    pub description: String,
    pub example: String,
    /// Whether this variable contains sensitive data (passwords, keys, tokens)
    pub sensitive: bool,
}

/// Information about an available Docker container that can be imported
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvailableContainer {
    /// Container ID or name
    pub container_id: String,
    /// Container name
    pub container_name: String,
    /// Docker image name (e.g., "postgres:17-alpine")
    pub image: String,
    /// Extracted version from image (e.g., "17")
    pub version: String,
    /// Service type this container represents
    pub service_type: ServiceType,
    /// Whether the container is currently running
    pub is_running: bool,
    /// Exposed ports (e.g., [5432] for PostgreSQL, [6379] for Redis)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exposed_ports: Vec<u16>,
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait ExternalService: Send + Sync {
    /// Initialize the service with given configuration
    /// Returns a HashMap of inferred parameters that should be stored
    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>>;

    /// Check if the service is healthy
    async fn health_check(&self) -> Result<bool>;

    /// Get service type
    fn get_type(&self) -> ServiceType;

    /// Get service name
    fn get_name(&self) -> String;

    /// Get connection string or endpoint
    fn get_connection_info(&self) -> Result<String>;

    /// Cleanup/shutdown the service
    async fn cleanup(&self) -> Result<()>;

    /// Get parameter schema as JSON Schema
    /// Services must implement this to provide their configuration schema
    fn get_parameter_schema(&self) -> Option<serde_json::Value>;

    /// Start the service
    async fn start(&self) -> Result<()>;

    /// Stop the service
    async fn stop(&self) -> Result<()>;

    /// Remove the service and its data completely
    async fn remove(&self) -> Result<()>;

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>>;

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>>;

    /// Provision a logical resource (like a database or schema) for a specific project and environment
    async fn provision_resource(
        &self,
        _service_config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<LogicalResource> {
        Ok(LogicalResource {
            name: format!("{}_{}", project_id, environment),
            resource_type: "default".to_string(),
            credentials: HashMap::new(),
        })
    }

    /// Deprovision a logical resource
    async fn deprovision_resource(&self, _project_id: &str, _environment: &str) -> Result<()> {
        Ok(())
    }

    /// Get definitions of environment variables that will be available at runtime
    fn get_runtime_env_definitions(&self) -> Vec<RuntimeEnvVar> {
        Vec::new()
    }

    /// Get actual runtime environment variables for a specific project/environment
    async fn get_runtime_env_vars(
        &self,
        _config: ServiceConfig,
        _project_id: &str,
        _environment: &str,
    ) -> Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String>;

    /// Get the effective host and port for connecting to this service
    /// In Docker mode, returns (container_name, internal_port)
    /// In Baremetal mode, returns (localhost, exposed_port)
    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)>;

    /// Backup the service data to an S3 location
    /// s3_source: The S3 source configuration to use for backup
    /// subpath: The subpath within the S3 bucket where the backup should be stored
    async fn backup_to_s3(
        &self,
        _s3_client: &aws_sdk_s3::Client,
        _backup: temps_entities::backups::Model,
        _s3_source: &temps_entities::s3_sources::Model,
        _subpath: &str,
        _subpath_root: &str,
        _pool: &temps_database::DbConnection,
        _external_service: &temps_entities::external_services::Model,
        _service_config: ServiceConfig,
    ) -> Result<String> {
        Err(anyhow::anyhow!("Backup not implemented for this service"))
    }

    /// Restore the service data from an S3 backup
    async fn restore_from_s3(
        &self,
        _s3_client: &aws_sdk_s3::Client,
        _backup_location: &str,
        _s3_source: &temps_entities::s3_sources::Model,
        _service_config: ServiceConfig,
    ) -> Result<()> {
        Err(anyhow::anyhow!("Restore not implemented for this service"))
    }

    /// Upgrade the service to a new version/image with data migration
    /// This method handles version-specific upgrade logic (e.g., pg_upgrade for PostgreSQL)
    ///
    /// # Arguments
    /// * `old_config` - Configuration of the current running service
    /// * `new_config` - Configuration with the new version/image
    ///
    /// # Returns
    /// * `Ok(())` if upgrade successful
    /// * `Err(...)` if upgrade failed or not supported
    async fn upgrade(&self, _old_config: ServiceConfig, _new_config: ServiceConfig) -> Result<()> {
        Err(anyhow::anyhow!("Upgrade not implemented for this service"))
    }

    /// Get the default/recommended Docker image and version for this service
    /// Returns (image_name, version) tuple
    fn get_default_docker_image(&self) -> (String, String) {
        ("".to_string(), "latest".to_string())
    }

    /// Get the currently running Docker image and version for this service
    /// Returns (image_name, version) tuple
    async fn get_current_docker_image(&self) -> Result<(String, String)> {
        Err(anyhow::anyhow!(
            "Getting current docker image not implemented for this service"
        ))
    }

    /// Get the default/recommended version for this service
    fn get_default_version(&self) -> String {
        "latest".to_string()
    }

    /// Get the currently running version for this service
    async fn get_current_version(&self) -> Result<String> {
        Err(anyhow::anyhow!(
            "Getting current version not implemented for this service"
        ))
    }

    /// Import an existing running Docker container as a managed service
    /// User provides container ID and necessary credentials/configuration
    ///
    /// # Arguments
    /// * `container_id` - Docker container ID or name of the running service
    /// * `service_name` - Name to register the service as in Temps
    /// * `credentials` - User-provided credentials (username, password, etc)
    /// * `additional_config` - Any additional configuration needed (ports, paths, etc)
    ///
    /// # Returns
    /// * Returns registered ServiceConfig with managed parameters
    async fn import_from_container(
        &self,
        _container_id: String,
        _service_name: String,
        _credentials: HashMap<String, String>,
        _additional_config: serde_json::Value,
    ) -> Result<ServiceConfig> {
        Err(anyhow::anyhow!("Import not implemented for this service"))
    }
}
