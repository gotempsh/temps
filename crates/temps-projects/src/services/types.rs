use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use temps_entities::source_type::SourceType;
use thiserror::Error;

#[derive(Debug, Clone, Serialize)]
pub struct EnvVarEnvironment {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ProjectStatistics {
    pub total_count: i64,
}

#[derive(Debug, Serialize)]
pub struct EnvVarWithEnvironments {
    pub id: i32,
    pub project_id: i32,
    pub key: String,
    pub value: String,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub environments: Vec<EnvVarEnvironment>,
}

#[derive(Deserialize)]
pub struct UpdateDeploymentSettingsRequest {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
}

#[derive(Serialize)]
pub struct Project {
    pub id: i32,
    pub slug: String,
    pub name: String,
    pub repo_name: Option<String>,
    pub repo_owner: Option<String>,
    pub directory: String,
    pub main_branch: String,
    pub preset: Option<String>,
    /// Preset-specific configuration (Dockerfile path, build context, etc.)
    pub preset_config: Option<serde_json::Value>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub automatic_deploy: bool,
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub performance_metrics_enabled: bool,
    pub last_deployment: Option<UtcDateTime>,
    pub project_type: String,
    pub use_default_wildcard: bool,
    pub custom_domain: Option<String>,
    pub is_public_repo: bool,
    pub git_url: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    pub is_on_demand: bool,
    pub deployment_config: Option<temps_entities::prelude::DeploymentConfig>,
    pub attack_mode: bool,
    pub ai_alert_summaries_enabled: Option<bool>,
    pub ai_debug_chat_enabled: Option<bool>,
    pub ai_write_actions_enabled: bool,
    pub ai_api_traffic_summary_enabled: Option<bool>,
    /// Opt-in for native error-tracking source context.
    pub error_source_context_enabled: bool,
    /// Auto-capture source root (relative to the checkout); None = build context.
    pub error_source_root: Option<String>,
    pub enable_preview_environments: bool,
    /// When true, newly-created preview environments default to on-demand mode.
    pub preview_envs_on_demand: bool,
    /// Idle timeout (seconds) for on-demand preview environments.
    pub preview_envs_idle_timeout_seconds: i32,
    /// Wake timeout (seconds) for on-demand preview environments.
    pub preview_envs_wake_timeout_seconds: i32,
    /// Source type for deployments (git, docker_image, or static_files)
    pub source_type: SourceType,
    /// Opt-in: also accept deployments from a source other than `source_type`,
    /// so a Git project can additionally be shipped from an uploaded archive
    /// (`drop`) without losing its repository. NULL/false means off.
    pub allow_alternate_sources: Option<bool>,
    /// GitLab webhook ID installed on the connected repository, if any.
    pub gitlab_webhook_id: Option<i32>,
    /// ADR-027 Phase 3: whether this project's traces appear in cross-project
    /// discovery results. Default true (consistent with OSS global-observability
    /// model). Operators can set false to suppress cross-project links to this
    /// project.
    pub cross_project_trace_sharing: bool,
    /// How long (hours) to retain built Docker images before nightly cleanup.
    /// None = use the system default (336 h / 14 days out of the box, from
    /// `AppSettings.image_retention.default_hours`).
    pub image_retention_hours: Option<i32>,
}

/// One environment variable supplied while creating a project.
///
/// Deserializes from either shape so clients written before `is_secret`
/// existed keep working unchanged:
///
/// * object — `{"key": "API_KEY", "value": "sk-...", "is_secret": true}`
/// * legacy tuple — `["API_KEY", "sk-..."]` (implies `is_secret: false`)
#[derive(Debug, Clone, Serialize)]
pub struct CreateProjectEnvVar {
    pub key: String,
    pub value: String,
    /// When true the value is write-only: it is encrypted at rest and the API
    /// never returns the plaintext again.
    pub is_secret: bool,
}

impl<'de> Deserialize<'de> for CreateProjectEnvVar {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Object {
                key: String,
                value: String,
                #[serde(default)]
                is_secret: bool,
            },
            Tuple(String, String),
        }

        Ok(match Repr::deserialize(deserializer)? {
            Repr::Object {
                key,
                value,
                is_secret,
            } => CreateProjectEnvVar {
                key,
                value,
                is_secret,
            },
            Repr::Tuple(key, value) => CreateProjectEnvVar {
                key,
                value,
                is_secret: false,
            },
        })
    }
}

impl CreateProjectEnvVar {
    /// Non-secret variable, the shape most callers want.
    pub fn plain(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            is_secret: false,
        }
    }
}

#[derive(Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub repo_name: Option<String>,
    pub repo_owner: Option<String>,
    pub directory: String,
    pub main_branch: String,
    pub preset: String,
    /// Preset-specific configuration (for Dockerfile preset, Nixpacks, etc.)
    pub preset_config: Option<serde_json::Value>,
    pub environment_variables: Option<Vec<CreateProjectEnvVar>>,
    pub automatic_deploy: bool,
    pub storage_service_ids: Vec<i32>,
    pub is_public_repo: Option<bool>,
    pub git_url: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    pub exposed_port: Option<i32>,
    /// Source type for deployments (git, docker_image, or static_files)
    #[serde(default)]
    pub source_type: SourceType,
    /// Bounded template provenance: a reviewed bundled slug or the fixed
    /// `custom` marker. Internal only; normal project-creation paths leave it
    /// unset and operator-defined slugs are never persisted here.
    #[serde(default)]
    pub template_slug: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateProjectFromTemplateRequest {
    pub project_name: String,
    pub github_owner: String,
    pub github_name: String,
    pub template_name: String,
    pub environment_variables: Option<Vec<CreateProjectEnvVar>>,
    pub automatic_deploy: Option<bool>,
    pub performance_metrics_enabled: Option<bool>,
    pub storage_service_ids: Vec<i32>,
}

#[derive(Debug, Serialize)]
pub struct CreateGithubRepoRequest {
    pub name: String,
    pub private: bool,
    #[serde(rename = "auto_init")]
    pub auto_init: bool,
}

// Types are defined directly in this file for simplicity

#[derive(Error, Debug)]
pub enum ProjectError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Project not found")]
    NotFound(String),

    #[error("Git provider connection {connection_id} not found or not accessible")]
    GitProviderConnectionNotFound { connection_id: i32 },

    #[error("Template not found")]
    TemplateNotFound,

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Slug already exists: {0}")]
    SlugAlreadyExists(String),

    #[error("A project with slug '{slug}' was created concurrently. Please retry.")]
    SlugConflict { slug: String },

    #[error("Failed to create default environment for project {project_id}: {reason}")]
    EnvironmentCreationFailed { project_id: i32, reason: String },

    #[error("Failed to create environment variable '{key}' for project {project_id}: {reason}")]
    EnvVarCreationFailed {
        project_id: i32,
        key: String,
        reason: String,
    },

    #[error("Failed to link storage service {service_id} to project {project_id}: {reason}")]
    StorageLinkFailed {
        project_id: i32,
        service_id: i32,
        reason: String,
    },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("GitHub error: {0}")]
    GitHubError(String),

    #[error("Deployment error: {0}")]
    DeploymentError(String),

    #[error("Failed to remove deployment containers for project {project_id}: {reason}")]
    DeploymentCleanupFailed { project_id: i32, reason: String },

    #[error(
        "Project {project_id} was saved, but its proxy routes could not be reloaded (queue: {queue_reason}; database notification: {database_reason})"
    )]
    RouteReloadFailed {
        project_id: i32,
        queue_reason: String,
        database_reason: String,
    },

    #[error("Other error: {0}")]
    Other(String),

    #[error("Pipeline error: {0}")]
    PipelineError(String),

    #[error("Invalid git URL '{url}': {reason}")]
    InvalidGitUrl { url: String, reason: String },
}

/// Detect a Postgres unique-violation regardless of the variant Sea-ORM
/// happens to wrap the underlying error in (Exec, Query, RecordNotInserted,
/// connection-level errors during insert). We check for SQLSTATE `23505` and
/// the textual marker `UNIQUE` / `duplicate key` so this works across
/// sqlx, runtime-tokio-rustls, and the legacy backends.
pub(crate) fn is_unique_violation(error: &sea_orm::DbErr) -> bool {
    if matches!(error, sea_orm::DbErr::RecordNotInserted) {
        return true;
    }
    let msg = error.to_string();
    msg.contains("23505")
        || msg.contains("duplicate key")
        || msg.contains("UNIQUE constraint")
        || msg.contains("UNIQUE violation")
}

impl From<sea_orm::DbErr> for ProjectError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(_) => ProjectError::NotFound(error.to_string()),
            ref e if is_unique_violation(e) => ProjectError::DatabaseError {
                reason: format!("Unique constraint violated: {}", error),
            },
            sea_orm::DbErr::Exec(ref err) if err.to_string().contains("FOREIGN KEY") => {
                ProjectError::DatabaseError {
                    reason: "A foreign key constraint was violated".to_string(),
                }
            }
            _ => ProjectError::DatabaseError {
                reason: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_project_env_var_accepts_object_form_with_secret_flag() {
        let parsed: CreateProjectEnvVar =
            serde_json::from_str(r#"{"key":"API_KEY","value":"sk-live","is_secret":true}"#)
                .expect("object form should deserialize");

        assert_eq!(parsed.key, "API_KEY");
        assert_eq!(parsed.value, "sk-live");
        assert!(parsed.is_secret);
    }

    #[test]
    fn create_project_env_var_object_form_defaults_is_secret_to_false() {
        let parsed: CreateProjectEnvVar = serde_json::from_str(r#"{"key":"PORT","value":"8080"}"#)
            .expect("is_secret should be optional");

        assert_eq!(parsed.key, "PORT");
        assert!(!parsed.is_secret);
    }

    #[test]
    fn create_project_env_var_accepts_legacy_tuple_form() {
        // Clients written before `is_secret` existed send `["KEY", "value"]`.
        let parsed: Vec<CreateProjectEnvVar> =
            serde_json::from_str(r#"[["PORT","8080"],["DEBUG","1"]]"#)
                .expect("legacy tuple form must keep working");

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].key, "PORT");
        assert_eq!(parsed[0].value, "8080");
        assert!(!parsed[0].is_secret);
        assert_eq!(parsed[1].key, "DEBUG");
        assert!(!parsed[1].is_secret);
    }

    #[test]
    fn create_project_env_var_mixes_both_forms_in_one_list() {
        let parsed: Vec<CreateProjectEnvVar> = serde_json::from_str(
            r#"[["PORT","8080"],{"key":"API_KEY","value":"sk","is_secret":true}]"#,
        )
        .expect("both forms should coexist");

        assert!(!parsed[0].is_secret);
        assert!(parsed[1].is_secret);
    }

    #[test]
    fn create_project_env_var_rejects_malformed_entries() {
        assert!(serde_json::from_str::<CreateProjectEnvVar>(r#"["ONLY_KEY"]"#).is_err());
        assert!(serde_json::from_str::<CreateProjectEnvVar>(r#"{"key":"K"}"#).is_err());
    }

    #[test]
    fn create_project_request_parses_env_vars_with_secrets() {
        let request: CreateProjectRequest = serde_json::from_str(
            r#"{
                "name": "api",
                "directory": ".",
                "main_branch": "main",
                "preset": "dockerfile",
                "environment_variables": [
                    {"key": "DATABASE_URL", "value": "postgres://x", "is_secret": true},
                    ["LOG_LEVEL", "info"]
                ],
                "automatic_deploy": true,
                "storage_service_ids": []
            }"#,
        )
        .expect("request should deserialize");

        let env_vars = request.environment_variables.expect("env vars present");
        assert_eq!(env_vars.len(), 2);
        assert!(env_vars[0].is_secret);
        assert!(!env_vars[1].is_secret);
    }
}
