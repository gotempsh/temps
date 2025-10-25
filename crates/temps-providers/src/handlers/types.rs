use crate::ExternalServiceManager;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use utoipa::ToSchema;

use temps_core::AuditLogger;

pub struct AppState {
    pub external_service_manager: Arc<ExternalServiceManager>,
    pub audit_service: Arc<dyn AuditLogger>,
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
}

impl ServiceTypeRoute {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "mongodb" => Ok(ServiceTypeRoute::Mongodb),
            "postgres" => Ok(ServiceTypeRoute::Postgres),
            "redis" => Ok(ServiceTypeRoute::Redis),
            "s3" => Ok(ServiceTypeRoute::S3),
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
    pub parameters: Vec<ServiceParameter>,
    pub current_parameters: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateExternalServiceRequest {
    pub name: String,
    pub service_type: ServiceTypeRoute,
    pub version: Option<String>,
    pub parameters: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateExternalServiceRequest {
    pub parameters: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LinkServiceRequest {
    pub project_id: i32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectServiceInfo {
    pub id: i32,
    pub project_id: i32,
    pub service: ExternalServiceInfo,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariableInfo {
    pub name: String,
    pub value: String,
}
