use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{AuditLogger, DeploymentCanceller};
use temps_entities::deployment_config::DeploymentConfig;
use utoipa::ToSchema;

use crate::services::env_var_service::EnvVarService;
use crate::services::environment_service::EnvironmentService;

pub struct AppState {
    pub environment_service: Arc<EnvironmentService>,
    pub env_var_service: Arc<EnvVarService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub deployment_service: Arc<dyn DeploymentCanceller>,
}

pub fn create_environment_app_state(
    environment_service: Arc<EnvironmentService>,
    env_var_service: Arc<EnvVarService>,
    audit_service: Arc<dyn AuditLogger>,
    deployment_service: Arc<dyn DeploymentCanceller>,
) -> Arc<AppState> {
    Arc::new(AppState {
        environment_service,
        env_var_service,
        audit_service,
        deployment_service,
    })
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateEnvironmentVariableRequest {
    pub key: String,
    pub value: String,
    pub environment_ids: Vec<i32>,
    /// Include this environment variable in preview environments (default: true)
    #[serde(default = "default_include_in_preview")]
    pub include_in_preview: bool,
}

fn default_include_in_preview() -> bool {
    true
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariableResponse {
    pub id: i32,
    pub key: String,
    pub value: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub environments: Vec<EnvironmentInfo>,
    /// Include this environment variable in preview environments
    pub include_in_preview: bool,
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
    pub branch: Option<String>,
    /// Indicates if this is a preview environment (auto-created per branch)
    /// For preview environments, 'branch' contains the feature branch name
    pub is_preview: bool,
    /// Deployment configuration for this environment (overrides project-level config)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_config: Option<DeploymentConfig>,
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
            branch: env.branch,
            is_preview: env.is_preview,
            deployment_config: env.deployment_config,
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
    /// Port exposed by the container (overrides project-level port for this environment)
    ///
    /// Priority order for port resolution:
    /// 1. Image EXPOSE directive (auto-detected from built image)
    /// 2. This environment-level exposed_port (overrides project setting)
    /// 3. Project-level exposed_port (fallback)
    /// 4. Default: 3000
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 8080)]
    pub exposed_port: Option<i32>,
    /// Enable/disable automatic deployments for this environment
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_deploy: Option<bool>,
    /// Enable/disable performance metrics collection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance_metrics_enabled: Option<bool>,
    /// Enable/disable session recording
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_recording_enabled: Option<bool>,
    /// Security configuration for this environment (overrides project-level settings)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<temps_entities::deployment_config::SecurityConfig>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateEnvironmentRequest {
    pub name: String,
    pub branch: String,
    /// If true, set this environment as the preview environment for the project
    #[serde(default)]
    pub set_as_preview: bool,
}
