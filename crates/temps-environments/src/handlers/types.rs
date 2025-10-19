use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::AuditLogger;
use utoipa::ToSchema;

use crate::services::environment_service::EnvironmentService;
use crate::services::env_var_service::EnvVarService;
pub struct AppState {
    pub environment_service: Arc<EnvironmentService>,
    pub env_var_service: Arc<EnvVarService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

pub fn create_environment_app_state(
    environment_service: Arc<EnvironmentService>,
    env_var_service: Arc<EnvVarService>,
    audit_service: Arc<dyn AuditLogger>,
) -> Arc<AppState> {
    Arc::new(AppState {
        environment_service,
        env_var_service,
        audit_service,
    })
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateEnvironmentVariableRequest {
    pub key: String,
    pub value: String,
    pub environment_ids: Vec<i32>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariableResponse {
    pub id: i32,
    pub key: String,
    pub value: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub environments: Vec<EnvironmentInfo>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct EnvironmentInfo {
    pub id: i32,
    pub name: String,
    pub main_url: String,
    pub current_deployment_id: Option<i32>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct GetEnvironmentVariablesQuery {
    pub environment_id: Option<i32>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct EnvironmentResponse {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub slug: String,
    pub main_url: String,
    pub current_deployment_id: Option<i32>,
    pub created_at: i64,
    pub updated_at: i64,
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub replicas: Option<i32>,
    pub branch: Option<String>,
}

impl From<temps_entities::environments::Model> for EnvironmentResponse {
    fn from(env: temps_entities::environments::Model) -> Self {
        Self {
            id: env.id,
            project_id: env.project_id,
            name: env.name,
            slug: env.slug,
            main_url: env.subdomain,
            current_deployment_id: env.current_deployment_id,
            created_at: env.created_at.timestamp_millis(),
            updated_at: env.updated_at.timestamp_millis(),
            cpu_request: env.cpu_request,
            cpu_limit: env.cpu_limit,
            memory_request: env.memory_request,
            memory_limit: env.memory_limit,
            replicas: env.replicas,
            branch: env.branch,
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct EnvironmentDomainResponse {
    pub id: i32,
    pub environment_id: i32,
    pub domain: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct AddEnvironmentDomainRequest {
    pub domain: String,
    pub is_primary: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariableValueResponse {
    pub value: String,
}


#[derive(Serialize, Deserialize, Clone, ToSchema)]
pub struct UpdateEnvironmentSettingsRequest {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub branch: Option<String>,
    pub replicas: Option<i32>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateEnvironmentRequest {
    pub name: String,
    pub branch: String,
}
