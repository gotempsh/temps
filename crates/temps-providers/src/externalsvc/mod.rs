use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

pub mod mongodb;
pub mod postgres;
pub mod redis;
pub mod s3;

// Re-export services for easier access
pub use mongodb::MongodbService;
pub use postgres::PostgresService;
pub use redis::RedisService;
pub use s3::S3Service;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub service_type: ServiceType,
    pub version: Option<String>,
    pub parameters: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Mongodb,
    Postgres,
    Redis,
    S3,
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceType::Mongodb => write!(f, "mongodb"),
            ServiceType::Postgres => write!(f, "postgres"),
            ServiceType::Redis => write!(f, "redis"),
            ServiceType::S3 => write!(f, "s3"),
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

    /// Get required parameters and their validation rules
    fn get_parameter_definitions(&self) -> Vec<ServiceParameter>;

    /// Validate parameters against the service's requirements
    fn validate_parameters(&self, parameters: &HashMap<String, String>) -> Result<()> {
        let required_params = self.get_parameter_definitions();

        // Check for required parameters
        for param in &required_params {
            if param.required && !parameters.contains_key(&param.name) {
                return Err(anyhow::anyhow!(
                    "Missing required parameter: {}",
                    param.name
                ));
            }
        }

        // Validate each provided parameter
        for (key, value) in parameters {
            if let Some(param) = required_params.iter().find(|p| p.name == *key) {
                // Check if value is in allowed choices if choices are defined
                if let Some(choices) = &param.choices {
                    if !choices.contains(value) {
                        return Err(anyhow::anyhow!(
                            "Parameter {} value '{}' is not one of the allowed choices: {:?}",
                            key,
                            value,
                            choices
                        ));
                    }
                }

                // Check validation pattern if it exists
                if let Some(pattern) = &param.validation_pattern {
                    let regex = regex::Regex::new(pattern)
                        .map_err(|_| anyhow::anyhow!("Invalid validation pattern for {}", key))?;

                    if !regex.is_match(value) {
                        return Err(anyhow::anyhow!(
                            "Parameter {} value does not match required pattern",
                            key
                        ));
                    }
                }
            } else {
                return Err(anyhow::anyhow!("Unknown parameter: {}", key));
            }
        }

        Ok(())
    }

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
}
