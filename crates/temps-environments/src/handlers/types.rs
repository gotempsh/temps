use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{AuditLogger, DeploymentCanceller, ProjectEnvVarsProvider};
use temps_entities::deployment_config::DeploymentConfig;
use utoipa::ToSchema;

use crate::services::env_var_service::EnvVarService;
use crate::services::environment_service::EnvironmentService;
use crate::services::secret_service::SecretService;

pub struct AppState {
    pub environment_service: Arc<EnvironmentService>,
    pub env_var_service: Arc<EnvVarService>,
    pub secret_service: Arc<SecretService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub deployment_service: Arc<dyn DeploymentCanceller>,
    /// Optional on-demand waker for starting/stopping containers during wake/sleep.
    /// Only available when the proxy's OnDemandManager is registered.
    pub on_demand_waker: Option<Arc<dyn temps_core::OnDemandWaker>>,
    /// Optional integration env-var provider. When absent (e.g. in tests without
    /// the providers plugin) the resolved view falls back to manual vars only.
    pub integration_env_provider: Option<Arc<dyn ProjectEnvVarsProvider>>,
}

pub fn create_environment_app_state(
    environment_service: Arc<EnvironmentService>,
    env_var_service: Arc<EnvVarService>,
    secret_service: Arc<SecretService>,
    audit_service: Arc<dyn AuditLogger>,
    deployment_service: Arc<dyn DeploymentCanceller>,
    on_demand_waker: Option<Arc<dyn temps_core::OnDemandWaker>>,
    integration_env_provider: Option<Arc<dyn ProjectEnvVarsProvider>>,
) -> Arc<AppState> {
    Arc::new(AppState {
        environment_service,
        env_var_service,
        secret_service,
        audit_service,
        deployment_service,
        on_demand_waker,
        integration_env_provider,
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

#[derive(Serialize, Deserialize, ToSchema, Clone)]
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
    /// When true, git pushes do NOT auto-deploy to this environment.
    /// Deployments must be promoted from another environment.
    pub protected: bool,
    /// When true, the environment's containers are currently stopped due to
    /// inactivity (on-demand mode) and will start on the next request.
    pub sleeping: bool,
    /// Last proxied request timestamp (epoch millis) for on-demand environments.
    /// NULL when on-demand is disabled or no traffic has been received yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<i64>,
    /// Estimated time (epoch millis) when the environment will go to sleep
    /// based on last activity + idle timeout. NULL when sleeping or on-demand disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_sleep_at: Option<i64>,
}

impl From<temps_entities::environments::Model> for EnvironmentResponse {
    fn from(env: temps_entities::environments::Model) -> Self {
        let last_activity_at = env.last_activity_at.map(|t| t.timestamp_millis());

        // Compute estimated sleep time: last_activity + idle_timeout
        // Only when on-demand is enabled, env is awake, and we have activity data
        let estimated_sleep_at = if !env.sleeping {
            env.deployment_config
                .as_ref()
                .filter(|dc| dc.on_demand)
                .and_then(|dc| {
                    env.last_activity_at.map(|last| {
                        last.timestamp_millis() + (dc.idle_timeout_seconds as i64 * 1000)
                    })
                })
        } else {
            None
        };

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
            protected: env.protected,
            sleeping: env.sleeping,
            last_activity_at,
            estimated_sleep_at,
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct EnvironmentDomainResponse {
    pub id: i32,
    pub environment_id: i32,
    pub domain: String,
    pub created_at: i64,
    /// Full URL for this domain (e.g., https://buildtolearndev-production.example.com)
    #[schema(example = "https://buildtolearndev-production.example.com")]
    pub url: String,
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

/// Where a resolved env var comes from. Integration-sourced vars may be
/// "shadowed" by a manual entry with the same key, in which case the response
/// carries `Manual` with `overrides_service` populated so the UI can still show
/// the integration icon.
#[derive(Serialize, Deserialize, ToSchema, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResolvedEnvVarSource {
    /// Manually-defined env var. If `overrides_service` is set, this key would
    /// otherwise have been supplied by an integration — the UI should show the
    /// integration icon plus an "overridden" indicator.
    Manual {
        var_id: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        overrides_service: Option<EnvVarIntegrationInfo>,
    },
    /// Supplied by a linked external service (Postgres, Redis, S3, etc.).
    Integration { service: EnvVarIntegrationInfo },
}

#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct EnvVarIntegrationInfo {
    pub service_id: i32,
    pub service_name: String,
    pub service_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_slug: Option<String>,
}

/// One entry in the computed env-var view that merges manual and integration
/// sources and tags each result with its origin. `value_preview` is always
/// masked — plaintext must be fetched per-key via the existing reveal endpoint,
/// which is audit-logged.
#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct ResolvedEnvVarResponse {
    pub key: String,
    /// Masked or truncated preview. Never the raw value.
    pub value_preview: String,
    pub source: ResolvedEnvVarSource,
    /// Environments this var applies to. For integration-sourced vars this
    /// reflects every environment of the project (integrations are global).
    pub environments: Vec<EnvironmentInfo>,
    /// Whether the var would be auto-applied to preview environments.
    /// Integration vars always surface in preview; manual vars follow the flag.
    pub include_in_preview: bool,
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
    /// Optional list of node IDs to deploy to (overrides project-level setting)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_nodes: Option<Vec<i32>>,
    /// Label selector for node-based scheduling (overrides project-level setting).
    /// Same key with array value -> OR, different keys -> AND.
    /// Example: `{"region": ["us", "asia"], "gpu": "true"}`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_labels: Option<serde_json::Value>,
    /// Anti-affinity: spread replicas across different nodes.
    /// When enabled, the scheduler avoids placing two replicas of the same
    /// environment on the same node. Defaults to `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anti_affinity: Option<bool>,
    /// When true, git pushes do NOT auto-deploy to this environment.
    /// Deployments must be promoted from another environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected: Option<bool>,
    /// Enable on-demand mode (scale-to-zero). Containers are stopped after
    /// idle_timeout_seconds of no traffic and started on the next request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_demand: Option<bool>,
    /// Seconds of inactivity before stopping containers (60-86400). Default: 300.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_timeout_seconds: Option<i32>,
    /// Max seconds to wait for containers to start on wake (5-120). Default: 30.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake_timeout_seconds: Option<i32>,
    /// Set a password to protect this environment. The proxy will show an HTML
    /// password form before allowing access. The password is bcrypt-hashed
    /// server-side and never stored in plaintext.
    /// Send an empty string to remove password protection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateEnvironmentRequest {
    pub name: String,
    pub branch: String,
    /// If true, set this environment as the preview environment for the project
    #[serde(default)]
    pub set_as_preview: bool,
}

/// Request to create a new project secret.
///
/// Project secrets are mounted into the container as files under
/// `/run/secrets/<KEY>` (mode 0400, tmpfs) instead of as environment variables.
/// Values are always encrypted at rest and never returned in plaintext from
/// the API after create. Distinct from agent secrets (global `/settings/secrets`).
#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateProjectSecretRequest {
    /// Identifier for the secret. Becomes the filename at `/run/secrets/<KEY>`.
    /// Must start with a letter or underscore and contain only A-Z, a-z, 0-9, _.
    pub key: String,
    /// Plaintext value, <= 1 MiB.
    pub value: String,
    #[serde(default)]
    pub environment_ids: Vec<i32>,
    /// Include this secret in preview environments.
    #[serde(default = "default_include_in_preview")]
    pub include_in_preview: bool,
}

/// Request to update a project secret. The `value` field is optional — omit it
/// to rotate only the environment scoping / preview flag without touching the
/// ciphertext.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateProjectSecretRequest {
    /// New plaintext value, <= 1 MiB. Omit to keep the existing value.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub environment_ids: Vec<i32>,
    #[serde(default = "default_include_in_preview")]
    pub include_in_preview: bool,
}

/// Project secret metadata. There is deliberately no `value` field — secret
/// plaintext is never returned after creation. Callers that need the value
/// must read it from the mounted file inside the container.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectSecretResponse {
    pub id: i32,
    pub project_id: i32,
    pub key: String,
    pub include_in_preview: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub environments: Vec<ProjectSecretEnvironmentInfo>,
}

#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct ProjectSecretEnvironmentInfo {
    pub id: i32,
    pub name: String,
    pub main_url: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct GetProjectSecretsQuery {
    pub environment_id: Option<i32>,
}
