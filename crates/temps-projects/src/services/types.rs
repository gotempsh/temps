// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    /// Internal presence bit used by upgrade validation. Secret list values
    /// are masked, so callers must never infer this from the serialized value.
    #[serde(skip)]
    pub has_value: bool,
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
    /// Bundled template provenance persisted on the project row.
    pub template_slug: Option<String>,
    /// Logo captured in the applied service-template release. This remains
    /// stable even when the live catalog changes.
    pub service_template_image_url: Option<String>,
    /// Exact applied service-template version.
    pub service_template_version: Option<String>,
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
    /// Provider type behind `git_provider_connection_id` (`github`,
    /// `github_app`, `gitlab`, `gitea`, `bitbucket`, `generic`), resolved from
    /// the connection's provider row. `None` when the project has no
    /// connection (public repo, Docker image, uploaded source) or when the
    /// caller fetched the project through a path that doesn't resolve it.
    ///
    /// This is the only authoritative answer to "which Git host is this
    /// project on" — the clone URL's hostname is not, because a self-hosted
    /// instance can live on any domain and a project may have no clone URL at
    /// all.
    pub git_provider_type: Option<String>,
    pub is_on_demand: bool,
    pub deployment_config: Option<temps_entities::prelude::DeploymentConfig>,
    pub attack_mode: bool,
    pub ai_alert_summaries_enabled: Option<bool>,
    pub ai_debug_chat_enabled: Option<bool>,
    pub ai_write_actions_enabled: bool,
    pub ai_api_traffic_summary_enabled: Option<bool>,
    /// Opt-in for native error-tracking source context.
    pub error_source_context_enabled: bool,
    /// Opt-in Trivy vulnerability scanning (post-deployment scan + daily rescans).
    /// Off by default — project owners explicitly enable it.
    pub vulnerability_scanning_enabled: bool,
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

/// Sparse set of project settings to change.
///
/// A struct rather than a positional parameter list: the settings endpoint
/// carries 22 optional fields, several of which share a type. `name` and `slug`
/// in particular are adjacent `Option<String>`s whose meanings are not
/// interchangeable — one is a display label, the other is the routing
/// identifier — and as positional arguments they could be transposed without a
/// compile error. Every field is `None` by default; only what is set changes.
#[derive(Debug, Default, Clone)]
pub struct UpdateProjectSettingsParams {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    pub main_branch: Option<String>,
    pub repo_owner: Option<String>,
    pub repo_name: Option<String>,
    pub preset: Option<String>,
    pub directory: Option<String>,
    pub attack_mode: Option<bool>,
    pub enable_preview_environments: Option<bool>,
    pub preview_envs_on_demand: Option<bool>,
    pub preview_envs_idle_timeout_seconds: Option<i32>,
    pub preview_envs_wake_timeout_seconds: Option<i32>,
    pub preset_config: Option<serde_json::Value>,
    pub ai_alert_summaries_enabled: Option<bool>,
    pub ai_debug_chat_enabled: Option<bool>,
    pub ai_write_actions_enabled: Option<bool>,
    pub cross_project_trace_sharing: Option<bool>,
    pub error_source_context_enabled: Option<bool>,
    pub vulnerability_scanning_enabled: Option<bool>,
    pub error_source_root: Option<String>,
    pub ai_api_traffic_summary_enabled: Option<bool>,
    /// Outer `None` leaves the retention window unchanged; `Some(None)` clears
    /// the per-project override back to the system default.
    pub image_retention_hours: Option<Option<i32>>,
}

/// A display-name change that actually happened.
///
/// Both ends are captured inside the transaction that performed the write and
/// under the same row lock, so the pair always describes one real transition.
/// Reading either half outside that lock would allow a concurrent rename to
/// supply one end of a transition whose other end came from a different
/// request.
#[derive(Debug, Clone)]
pub struct ProjectRename {
    /// The display name immediately before this update.
    pub from: String,
    /// The display name this update wrote.
    pub to: String,
}

/// Outcome of a project settings update.
///
/// Carries what the service actually *did* rather than leaving the caller to
/// infer it by reading the row before and after. A caller comparing its own
/// pre-read against the result would be racing any concurrent update: two
/// overlapping renames could pair one request's stale pre-read with the other's
/// persisted name and report a transition that never happened.
pub struct ProjectSettingsUpdate {
    /// The project as it stands after the update.
    pub project: Project,
    /// The rename this update performed, or `None` when no name was supplied
    /// or the supplied name matched what was already stored.
    pub rename: Option<ProjectRename>,
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
    /// When true the value is encrypted at rest and masked in list responses.
    /// Plaintext access requires the audited reveal endpoint.
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
    pub expected_slug: Option<String>,
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
    /// Services whose authorization relied on their one-time creator claim.
    /// Internal only: handlers populate this after access checks.
    #[serde(default)]
    pub storage_service_claim_ids: Vec<i32>,
    #[serde(default)]
    pub storage_service_claim_user_id: Option<i32>,
    pub is_public_repo: Option<bool>,
    pub git_url: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    pub exposed_port: Option<i32>,
    /// Optional curated-template resource profile. Generic callers leave
    /// these unset and receive the platform defaults.
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    /// Source type for deployments (git, docker_image, or static_files)
    #[serde(default)]
    pub source_type: SourceType,
    /// Bounded template provenance: a reviewed bundled slug or the fixed
    /// `custom` marker. Internal only; operator-defined slugs are never
    /// persisted here.
    #[serde(default)]
    pub template_slug: Option<String>,
}

#[derive(Error, Debug)]
pub enum ProjectError {
    #[error("Database connection error: {0}")]
    DatabaseConnectionError(String),

    #[error("Project not found")]
    NotFound(String),

    #[error("Git provider connection {connection_id} not found or not accessible")]
    GitProviderConnectionNotFound { connection_id: i32 },

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

    #[error("Failed to link storage services {service_ids:?} to project {project_id}: {reason}")]
    StorageLinksFailed {
        project_id: i32,
        service_ids: Vec<i32>,
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

    /// The proxy route-reload signal could not be published, so the write that
    /// changed the public ports was rolled back.
    ///
    /// `rolled_back_scope` describes *what* was rolled back, because it differs
    /// by caller and an operator debugging this alone cannot see which path they
    /// hit. `update_git_settings` performs all of its writes in the one
    /// transaction, so nothing survives. `update_project_settings` is a chain of
    /// independent commits and only its final transaction rolls back — earlier
    /// steps in the same request may already be persisted. Claiming "nothing was
    /// saved" there would be false.
    #[error(
        "Project {project_id}'s proxy routes could not be signalled for reload. \
         {rolled_back_scope} Retry once the database is reachable."
    )]
    RouteReloadFailed {
        project_id: i32,
        rolled_back_scope: String,
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
