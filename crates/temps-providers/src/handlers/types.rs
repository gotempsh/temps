use crate::{ExternalServiceManager, QueryService};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use utoipa::ToSchema;

use temps_core::AuditLogger;

pub struct AppState {
    pub external_service_manager: Arc<ExternalServiceManager>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub query_service: Arc<QueryService>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceParameter {
    pub name: String,
    pub required: bool,
    pub encrypted: bool,
    pub description: String,
    pub default_value: Option<String>,
    pub validation_pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
}

impl From<crate::externalsvc::ServiceParameter> for ServiceParameter {
    fn from(param: crate::externalsvc::ServiceParameter) -> Self {
        Self {
            name: param.name,
            required: param.required,
            encrypted: param.encrypted,
            description: param.description,
            default_value: param.default_value,
            validation_pattern: param.validation_pattern,
            choices: param.choices,
        }
    }
}

impl From<ServiceParameter> for crate::externalsvc::ServiceParameter {
    fn from(param: ServiceParameter) -> Self {
        Self {
            name: param.name,
            required: param.required,
            encrypted: param.encrypted,
            description: param.description,
            default_value: param.default_value,
            validation_pattern: param.validation_pattern,
            choices: param.choices,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTypeRoute {
    Mongodb,
    Postgres,
    Redis,
    S3,
    /// Temps KV service (Redis-backed key-value store)
    Kv,
    /// Temps Blob service (RustFS-backed object storage)
    Blob,
    /// RustFS S3-compatible object storage (standalone)
    Rustfs,
}

impl ServiceTypeRoute {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "mongodb" => Ok(ServiceTypeRoute::Mongodb),
            "postgres" => Ok(ServiceTypeRoute::Postgres),
            "redis" => Ok(ServiceTypeRoute::Redis),
            "s3" => Ok(ServiceTypeRoute::S3),
            "kv" => Ok(ServiceTypeRoute::Kv),
            "blob" => Ok(ServiceTypeRoute::Blob),
            "rustfs" => Ok(ServiceTypeRoute::Rustfs),
            _ => Err(anyhow::anyhow!("Invalid service type: {}", s)),
        }
    }

    /// Returns a Vec containing all available service types
    pub fn get_all() -> Vec<ServiceTypeRoute> {
        vec![
            ServiceTypeRoute::Mongodb,
            ServiceTypeRoute::Postgres,
            ServiceTypeRoute::Redis,
            ServiceTypeRoute::S3,
            ServiceTypeRoute::Kv,
            ServiceTypeRoute::Blob,
            ServiceTypeRoute::Rustfs,
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
impl std::fmt::Display for ServiceTypeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceTypeRoute::Mongodb => write!(f, "mongodb"),
            ServiceTypeRoute::Postgres => write!(f, "postgres"),
            ServiceTypeRoute::Redis => write!(f, "redis"),
            ServiceTypeRoute::S3 => write!(f, "s3"),
            ServiceTypeRoute::Kv => write!(f, "kv"),
            ServiceTypeRoute::Blob => write!(f, "blob"),
            ServiceTypeRoute::Rustfs => write!(f, "rustfs"),
        }
    }
}

impl From<ServiceTypeRoute> for crate::externalsvc::ServiceType {
    fn from(service_type: ServiceTypeRoute) -> Self {
        match service_type {
            ServiceTypeRoute::Mongodb => crate::externalsvc::ServiceType::Mongodb,
            ServiceTypeRoute::Postgres => crate::externalsvc::ServiceType::Postgres,
            ServiceTypeRoute::Redis => crate::externalsvc::ServiceType::Redis,
            ServiceTypeRoute::S3 => crate::externalsvc::ServiceType::S3,
            ServiceTypeRoute::Kv => crate::externalsvc::ServiceType::Kv,
            ServiceTypeRoute::Blob => crate::externalsvc::ServiceType::Blob,
            ServiceTypeRoute::Rustfs => crate::externalsvc::ServiceType::Rustfs,
        }
    }
}

impl From<crate::externalsvc::ServiceType> for ServiceTypeRoute {
    fn from(service_type: crate::externalsvc::ServiceType) -> Self {
        match service_type {
            crate::externalsvc::ServiceType::Mongodb => ServiceTypeRoute::Mongodb,
            crate::externalsvc::ServiceType::Postgres => ServiceTypeRoute::Postgres,
            crate::externalsvc::ServiceType::Redis => ServiceTypeRoute::Redis,
            crate::externalsvc::ServiceType::S3 => ServiceTypeRoute::S3,
            crate::externalsvc::ServiceType::Kv => ServiceTypeRoute::Kv,
            crate::externalsvc::ServiceType::Blob => ServiceTypeRoute::Blob,
            crate::externalsvc::ServiceType::Rustfs => ServiceTypeRoute::Rustfs,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ExternalServiceInfo {
    pub id: i32,
    pub name: String,
    pub service_type: ServiceTypeRoute,
    pub version: Option<String>,
    pub status: String,
    pub connection_info: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ServiceTypeInfo {
    #[schema(example = "postgres")]
    pub service_type: ServiceTypeRoute,
    #[schema(
        example = "[{\"name\": \"host\", \"required\": true, \"encrypted\": false, \"description\": \"Database host\"}]"
    )]
    pub parameters: Vec<ServiceParameter>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProviderMetadata {
    #[schema(example = "postgres")]
    pub service_type: ServiceTypeRoute,
    #[schema(example = "PostgreSQL")]
    pub display_name: String,
    #[schema(example = "Relational database management system")]
    pub description: String,
    #[schema(example = "https://cdn.simpleicons.org/postgresql")]
    pub icon_url: String,
    #[schema(example = "#336791")]
    pub color: String,
}

impl ProviderMetadata {
    pub fn get_all() -> Vec<Self> {
        vec![
            Self {
                service_type: ServiceTypeRoute::Mongodb,
                display_name: "MongoDB".to_string(),
                description: "NoSQL document database".to_string(),
                icon_url: "/providers/mongodb.svg".to_string(),
                color: "#47A248".to_string(),
            },
            Self {
                service_type: ServiceTypeRoute::Postgres,
                display_name: "PostgreSQL".to_string(),
                description: "Relational database management system".to_string(),
                icon_url: "/providers/postgresql.svg".to_string(),
                color: "#4169E1".to_string(),
            },
            Self {
                service_type: ServiceTypeRoute::Redis,
                display_name: "Redis".to_string(),
                description: "In-memory data structure store".to_string(),
                icon_url: "/providers/redis.svg".to_string(),
                color: "#DC382D".to_string(),
            },
            Self {
                service_type: ServiceTypeRoute::S3,
                display_name: "S3 / MinIO".to_string(),
                description: "Object storage service".to_string(),
                icon_url: "/providers/s3.svg".to_string(),
                color: "#C72E49".to_string(),
            },
        ]
    }

    pub fn get_by_type(service_type: &ServiceTypeRoute) -> Option<Self> {
        Self::get_all()
            .into_iter()
            .find(|p| &p.service_type == service_type)
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ExternalServiceDetails {
    pub service: ExternalServiceInfo,
    pub parameter_schema: Option<serde_json::Value>,
    pub current_parameters: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateExternalServiceRequest {
    pub name: String,
    pub service_type: ServiceTypeRoute,
    pub version: Option<String>,
    pub parameters: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateExternalServiceRequest {
    pub parameters: HashMap<String, serde_json::Value>,
    /// Docker image to use for the service (e.g., "postgres:17-alpine", "timescale/timescaledb-ha:pg17")
    /// When provided, the service will be recreated with the new image while preserving data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_image: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpgradeExternalServiceRequest {
    /// Docker image to upgrade to (e.g., "postgres:17-alpine")
    /// This will trigger pg_upgrade for PostgreSQL or equivalent upgrade procedures for other services
    #[schema(example = "postgres:17-alpine")]
    pub docker_image: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LinkServiceRequest {
    pub project_id: i32,
}

/// Available Docker container that can be imported as a service
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvailableContainerInfo {
    /// Container ID or name
    #[schema(example = "abc123def456")]
    pub container_id: String,
    /// Container display name
    #[schema(example = "my-postgres")]
    pub container_name: String,
    /// Docker image name (e.g., "postgres:17-alpine")
    #[schema(example = "postgres:17-alpine")]
    pub image: String,
    /// Extracted version from image
    #[schema(example = "17")]
    pub version: String,
    /// Service type this container represents
    pub service_type: ServiceTypeRoute,
    /// Whether the container is currently running
    #[schema(example = true)]
    pub is_running: bool,
    /// Exposed ports (e.g., [5432] for PostgreSQL, [6379] for Redis)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exposed_ports: Vec<u16>,
}

/// Request to import a Docker container as a managed service
#[derive(Debug, Deserialize, ToSchema)]
pub struct ImportExternalServiceRequest {
    /// Name to register the service as in Temps
    #[schema(example = "production-database")]
    pub name: String,
    /// Service type
    pub service_type: ServiceTypeRoute,
    /// Optional version override
    pub version: Option<String>,
    /// Service configuration parameters
    pub parameters: HashMap<String, serde_json::Value>,
    /// Container ID or name to import
    #[schema(example = "abc123def456")]
    pub container_id: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectInfo {
    pub id: i32,
    pub slug: String,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectServiceInfo {
    pub id: i32,
    pub project: ProjectInfo,
    pub service: ExternalServiceInfo,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariableInfo {
    pub name: String,
    pub value: String,
    /// Whether this variable contains sensitive data (passwords, keys, tokens)
    #[schema(example = false)]
    pub sensitive: bool,
}
