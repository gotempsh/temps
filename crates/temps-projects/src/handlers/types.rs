use serde::{Deserialize, Serialize};
use temps_core::templates::TemplateService;
use temps_core::UtcDateTime;
use temps_entities::deployment_config::DeploymentConfig;
use temps_entities::source_type::SourceType;
use utoipa::ToSchema;

use crate::services::custom_domains::CustomDomainService;
use crate::services::project::ProjectService;
use crate::services::types::{CreateProjectEnvVar, ProjectError};
use http::StatusCode;
use std::sync::Arc;
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;
use temps_core::AuditLogger;
use temps_presets::preset_config_schema::PresetConfigSchema;

pub struct AppState {
    pub project_service: Arc<ProjectService>,
    pub deployment_canceller: Arc<dyn temps_core::DeploymentCanceller>,
    pub deployment_container_cleaner: Arc<dyn temps_core::DeploymentContainerCleaner>,
    pub custom_domain_service: Arc<CustomDomainService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub template_service: Arc<TemplateService>,
    pub project_archive_cleaner: Arc<dyn temps_core::ProjectArchiveCleaner>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    /// Optional checker enforcing team-based project access for human sessions.
    ///
    /// `None` when no plugin implementing this check is registered — the
    /// `project_access_guard!` macro is a strict synchronous no-op in that
    /// case, matching the behaviour of `deployment_gate` in `temps-deployments`.
    /// Resolved once in `configure_routes` via
    /// `context.get_service::<dyn temps_core::ProjectAccessChecker>()`, which
    /// is guaranteed to see all registered services because `configure_routes`
    /// runs after every plugin's `initialize_plugin_services` has completed.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

// Domain-related types
#[derive(Debug, Serialize)]
pub struct DomainInfo {
    pub id: i32,
    pub domain: String,
    pub expiration_time: Option<UtcDateTime>,
    pub last_renewed: Option<UtcDateTime>,
}

#[derive(Debug, Serialize)]
pub struct DomainEnvironment {
    pub id: i32,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Serialize)]
pub struct CustomDomainWithInfo {
    pub custom_domain: temps_entities::project_custom_domains::Model,
    pub domain_info: Option<DomainInfo>,
    pub environment: Option<DomainEnvironment>,
}

// Deployment-related types
#[derive(Debug, Serialize)]
pub struct Deployment {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment: DeploymentEnvironment,
    pub status: String,
    pub url: String,
    pub commit_hash: Option<String>,
    pub commit_message: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub created_at: UtcDateTime,
    pub screenshot_location: Option<String>,
    pub commit_author: Option<String>,
    pub commit_date: Option<UtcDateTime>,
    pub is_current: bool,
    pub message: Option<String>,
    pub pipeline: DeploymentPipeline,
}

#[derive(Debug, Serialize)]
pub struct DeploymentListResponse {
    pub deployments: Vec<Deployment>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

#[derive(Debug, Serialize)]
pub struct DeploymentDomain {
    pub id: i32,
    pub domain: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentEnvironment {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub domains: Vec<String>,
    pub main_url: String,
    pub current_deployment_id: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct DeploymentPipeline {
    pub id: i32,
    pub status: String,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub log_id: String,
    pub result: Option<serde_json::Value>,
    pub started_at: Option<UtcDateTime>,
    pub finished_at: Option<UtcDateTime>,
    pub branch_ref: Option<String>,
    pub tag_ref: Option<String>,
    pub commit_sha: Option<String>,
    pub commit_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeploymentStage {
    pub id: i32,
    pub deployment_id: i32,
    pub stage_name: String,
    pub status: String,
    pub log_file_path: String,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub started_at: Option<UtcDateTime>,
    pub finished_at: Option<UtcDateTime>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentDomainResponse {
    pub id: i32,
    pub domain: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectList {
    pub projects: Vec<ProjectResponse>,
}

/// Documentation-only schema for a project-creation environment variable.
///
/// The wire format is deserialized by [`CreateProjectEnvVar`], which also
/// accepts the legacy `["KEY", "value"]` tuple form. This struct exists so the
/// OpenAPI spec (and the generated clients) describe the preferred object form
/// with its `is_secret` flag.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectEnvVarInput {
    /// Variable name, e.g. `DATABASE_URL`
    pub key: String,
    /// Variable value
    pub value: String,
    /// Mark the variable as a write-only secret. Secret values are encrypted at
    /// rest and never returned in plaintext by the API — they can only be
    /// replaced, not read back. Defaults to `false`.
    #[serde(default)]
    pub is_secret: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateProjectRequest {
    pub name: String,
    pub repo_name: Option<String>,
    pub repo_owner: Option<String>,
    pub directory: String,
    pub main_branch: String,
    pub preset: String,
    /// Preset-specific configuration
    ///
    /// Different presets accept different configuration options:
    /// - **Dockerfile preset**: Accepts `DockerfilePresetConfig` with `dockerfile_path` and `build_context`
    /// - **Nixpacks preset**: Accepts ordered `providers` (for example `["...", "python"]`)
    ///   and optional inline `nixpacksConfig` TOML
    /// - **Static presets** (Vite, Next.js, etc.): Accept `StaticPresetConfig` with build commands and output dir
    ///
    /// Example for Dockerfile preset:
    /// ```json
    /// {
    ///   "dockerfilePath": "docker/Dockerfile",
    ///   "buildContext": "./api"
    /// }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<PresetConfigSchema>)]
    pub preset_config: Option<serde_json::Value>,
    pub output_dir: Option<String>,
    pub build_command: Option<String>,
    pub install_command: Option<String>,
    /// Environment variables to seed the default (production) environment with.
    ///
    /// Accepts objects — `{"key": "API_KEY", "value": "sk-...", "is_secret": true}`
    /// — or the legacy two-element form `["API_KEY", "sk-..."]`, which implies
    /// `is_secret: false`.
    #[schema(value_type = Option<Vec<ProjectEnvVarInput>>)]
    pub environment_variables: Option<Vec<CreateProjectEnvVar>>,
    pub automatic_deploy: Option<bool>,
    pub project_type: Option<String>,
    pub is_web_app: Option<bool>,
    #[serde(default = "default_performance_metrics")]
    pub performance_metrics_enabled: bool,
    pub storage_service_ids: Vec<i32>,
    pub use_default_wildcard: Option<bool>,
    pub custom_domain: Option<String>,
    pub is_public_repo: Option<bool>,
    pub git_url: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    pub is_on_demand: Option<bool>,
    /// Port exposed by the container (fallback when image has no EXPOSE directive)
    ///
    /// Priority order for port resolution:
    /// 1. Image EXPOSE directive (auto-detected from built image)
    /// 2. Environment-level exposed_port (overrides this value per environment)
    /// 3. This project-level exposed_port (fallback)
    /// 4. Default: 3000
    ///
    /// Only set this if your image doesn't use EXPOSE directive.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 8080)]
    pub exposed_port: Option<i32>,
    /// Source type for deployments
    ///
    /// Determines how the project is deployed:
    /// - **git** (default): Traditional Git-based deployments - source code is pulled, built, and deployed
    /// - **docker_image**: Deploy pre-built Docker images from external registries (DockerHub, GHCR, etc.)
    /// - **static_files**: Deploy pre-built static files uploaded as tar.gz or zip bundles
    ///
    /// For `docker_image` and `static_files` source types, `repo_name` and `repo_owner` are optional.
    #[serde(default)]
    pub source_type: SourceType,
}

/// Change a project's source type to a Git-less type (docker_image /
/// static_files / manual). Switching TO `git` is done via the Git settings
/// endpoint (which also supplies the repository + provider connection).
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ChangeProjectSourceRequest {
    pub source_type: SourceType,
}

/// Opt a project in or out of accepting deployments from a source other than
/// its configured `source_type`.
///
/// Unlike `ChangeProjectSourceRequest` this leaves `source_type` alone, so a
/// Git project keeps its repository, branch, webhook auto-deploy and
/// rollback-rebuild behaviour and merely gains the ability to also be deployed
/// from an uploaded source archive.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct SetAlternateSourcesRequest {
    /// `true` to also accept uploaded source archives, `false` to restrict the
    /// project to its configured source again.
    pub allow_alternate_sources: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TriggerPipelinePayload {
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub commit: Option<String>,
    /// Optional environment ID - if not provided, will use the project's preview environment
    pub environment_id: Option<i32>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TriggerPipelineResponse {
    pub message: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub commit: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectRecommendationsResponse {
    pub is_on_demand_recommended: bool,
    pub automatic_deploy_recommended: bool,
    pub git_provider_valid: bool,
    pub recommendations: Vec<String>,
}

fn default_performance_metrics() -> bool {
    true
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct PaginationParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct PaginatedProjectList {
    pub projects: Vec<ProjectResponse>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectResponse {
    pub id: i32,
    pub slug: String,
    pub name: String,
    pub repo_name: Option<String>,
    pub repo_owner: Option<String>,
    pub directory: String,
    pub main_branch: String,
    pub preset: Option<String>,
    /// Preset-specific configuration (Dockerfile path, build context, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_config: Option<serde_json::Value>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_deployment: Option<i64>,
    pub git_provider_connection_id: Option<i32>,
    /// Authoritative repository visibility. A missing connection alone does
    /// not imply that an incompletely configured repository is public.
    pub is_public_repo: bool,
    /// Git clone URL for the repository (used for public repos without a provider connection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    /// Deployment configuration (resources, autoscaling, features)
    pub deployment_config: DeploymentConfig,
    /// Attack mode - when enabled, requires CAPTCHA verification for all project environments
    pub attack_mode: bool,
    /// Opt-in to AI summarization of metric alert notifications (NULL/false = off).
    pub ai_alert_summaries_enabled: Option<bool>,
    /// Opt-in to AI debugging chat, e.g. on deployment failures (NULL/false = off).
    pub ai_debug_chat_enabled: Option<bool>,
    /// Opt-in to AI propose-then-confirm write capability (false = off).
    pub ai_write_actions_enabled: bool,
    /// Opt-in to AI summarization of API traffic analytics (NULL/false = off).
    pub ai_api_traffic_summary_enabled: Option<bool>,
    /// Opt-in to native error-tracking source context (false = off). When on,
    /// Temps stores uploaded source files and shows source code in stack traces.
    pub error_source_context_enabled: bool,
    /// Where auto-capture reads source from (relative to the checkout). Null =
    /// the deployment's Docker build context.
    pub error_source_root: Option<String>,
    /// Enable automatic preview environment creation for each branch
    pub enable_preview_environments: bool,
    /// When true, newly-created preview environments default to on-demand mode
    /// (containers stop after the configured idle timeout to save resources).
    pub preview_envs_on_demand: bool,
    /// Idle timeout (seconds) for on-demand preview environments.
    pub preview_envs_idle_timeout_seconds: i32,
    /// Wake timeout (seconds) for on-demand preview environments.
    pub preview_envs_wake_timeout_seconds: i32,
    /// Source type for deployments (git, docker_image, or static_files)
    pub source_type: SourceType,
    /// Whether this project also accepts deployments from a source other than
    /// `source_type` — chiefly, whether a Git-backed project will take an
    /// uploaded source archive (`drop`). `null` or `false` means only the
    /// configured `source_type` (plus Docker images and static bundles, which
    /// every project accepts) may be deployed.
    #[schema(example = false)]
    pub allow_alternate_sources: Option<bool>,
    /// GitLab webhook ID installed on the connected repository.
    /// `null` when no GitLab webhook is installed (not connected to GitLab,
    /// or webhook was removed / never created).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 42)]
    pub gitlab_webhook_id: Option<i32>,
    /// ADR-027 Phase 3 opt-out: when false, this project's traces are suppressed
    /// from cross-project discovery results. Default true (consistent with the
    /// OSS global-observability model where any OtelRead holder can query any
    /// project's telemetry).
    pub cross_project_trace_sharing: bool,
    /// Hours to retain built Docker images before nightly cleanup. Null = use the
    /// system-wide default from settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_retention_hours: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentDomains {
    pub domains: Vec<String>,
    pub environment_id: i32,
    pub environment_slug: String,
}

impl ProjectResponse {
    pub fn map_from_project(project: crate::services::types::Project) -> Self {
        ProjectResponse {
            id: project.id,
            slug: project.slug,
            name: project.name,
            repo_name: project.repo_name,
            repo_owner: project.repo_owner,
            directory: project.directory,
            main_branch: project.main_branch,
            preset: project.preset,
            preset_config: project.preset_config,
            created_at: project.created_at.timestamp_millis(),
            updated_at: project.updated_at.timestamp_millis(),
            last_deployment: project.last_deployment.map(|d| d.timestamp_millis()),
            git_provider_connection_id: project.git_provider_connection_id,
            is_public_repo: project.is_public_repo,
            git_url: project.git_url,
            attack_mode: project.attack_mode,
            ai_alert_summaries_enabled: project.ai_alert_summaries_enabled,
            ai_debug_chat_enabled: project.ai_debug_chat_enabled,
            ai_write_actions_enabled: project.ai_write_actions_enabled,
            ai_api_traffic_summary_enabled: project.ai_api_traffic_summary_enabled,
            error_source_context_enabled: project.error_source_context_enabled,
            error_source_root: project.error_source_root,
            enable_preview_environments: project.enable_preview_environments,
            preview_envs_on_demand: project.preview_envs_on_demand,
            preview_envs_idle_timeout_seconds: project.preview_envs_idle_timeout_seconds,
            preview_envs_wake_timeout_seconds: project.preview_envs_wake_timeout_seconds,
            source_type: project.source_type,
            allow_alternate_sources: project.allow_alternate_sources,
            gitlab_webhook_id: project.gitlab_webhook_id,
            cross_project_trace_sharing: project.cross_project_trace_sharing,
            image_retention_hours: project.image_retention_hours,
            deployment_config: DeploymentConfig {
                cpu_request: project
                    .deployment_config
                    .clone()
                    .map(|c| c.cpu_request)
                    .unwrap_or(None),
                cpu_limit: project
                    .deployment_config
                    .clone()
                    .map(|c| c.cpu_limit)
                    .unwrap_or(None),
                memory_request: project
                    .deployment_config
                    .clone()
                    .map(|c| c.memory_request)
                    .unwrap_or(None),
                memory_limit: project
                    .deployment_config
                    .clone()
                    .map(|c| c.memory_limit)
                    .unwrap_or(None),
                exposed_port: project
                    .deployment_config
                    .clone()
                    .map(|c| c.exposed_port)
                    .unwrap_or(None), // Not exposed in old Project struct
                automatic_deploy: project
                    .deployment_config
                    .clone()
                    .and_then(|c| c.automatic_deploy),
                performance_metrics_enabled: project
                    .deployment_config
                    .clone()
                    .map(|c| c.performance_metrics_enabled)
                    .unwrap_or(false),
                session_recording_enabled: project
                    .deployment_config
                    .clone()
                    .map(|c| c.session_recording_enabled)
                    .unwrap_or(false), // Default for old projects
                replicas: project
                    .deployment_config
                    .clone()
                    .map(|c| c.replicas)
                    .unwrap_or(1), // Default
                security: project.deployment_config.clone().and_then(|c| c.security),
                target_nodes: project
                    .deployment_config
                    .clone()
                    .and_then(|c| c.target_nodes),
                target_labels: project
                    .deployment_config
                    .clone()
                    .and_then(|c| c.target_labels),
                anti_affinity: project
                    .deployment_config
                    .clone()
                    .map(|c| c.anti_affinity)
                    .unwrap_or(true),
                on_demand: project
                    .deployment_config
                    .clone()
                    .map(|c| c.on_demand)
                    .unwrap_or(false),
                idle_timeout_seconds: project
                    .deployment_config
                    .clone()
                    .map(|c| c.idle_timeout_seconds)
                    .unwrap_or(300),
                wake_timeout_seconds: project
                    .deployment_config
                    .clone()
                    .map(|c| c.wake_timeout_seconds)
                    .unwrap_or(30),
                request_timeout_seconds: project
                    .deployment_config
                    .clone()
                    .and_then(|c| c.request_timeout_seconds),
                sse_idle_timeout_seconds: project
                    .deployment_config
                    .clone()
                    .and_then(|c| c.sse_idle_timeout_seconds),
                websocket_idle_timeout_seconds: project
                    .deployment_config
                    .clone()
                    .and_then(|c| c.websocket_idle_timeout_seconds),
                max_concurrent_connections: project
                    .deployment_config
                    .clone()
                    .and_then(|c| c.max_concurrent_connections),
                container_exec_enabled: project
                    .deployment_config
                    .clone()
                    .map(|c| c.container_exec_enabled)
                    .unwrap_or(false),
                cross_architecture_builds: project
                    .deployment_config
                    .clone()
                    .and_then(|c| c.cross_architecture_builds),
            },
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CustomDomainRequest {
    pub domain: String,
    pub redirect_to: Option<String>,
    pub status_code: Option<i32>,
    pub branch: Option<String>,
    pub environment_id: i32,
    /// Docker Compose service name this domain routes to (only for docker-compose projects)
    pub service_name: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ReassignCustomDomainRequest {
    pub target_project_id: i32,
    pub target_environment_id: i32,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DomainEnvironmentResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CustomDomainResponse {
    pub id: i32,
    pub project_id: i32,
    pub domain: String,
    pub domain_id: Option<i32>,
    pub environment: Option<DomainEnvironmentResponse>,
    pub redirect_to: Option<String>,
    pub status_code: Option<i32>,
    pub branch: Option<String>,
    pub status: String,
    pub message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub expiration_time: Option<i64>,
    pub last_renewed: Option<i64>,
    /// Docker Compose service name this domain routes to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
}

impl From<CustomDomainWithInfo> for CustomDomainResponse {
    fn from(domain_with_info: CustomDomainWithInfo) -> Self {
        let domain = domain_with_info.custom_domain;
        let domain_info = domain_with_info.domain_info;

        CustomDomainResponse {
            id: domain.id,
            project_id: domain.project_id,
            domain: domain.domain,
            domain_id: domain_info.as_ref().map(|info| info.id),
            environment: match domain_with_info.environment {
                Some(env) => Some(DomainEnvironmentResponse {
                    id: env.id,
                    name: env.name,
                    slug: env.slug,
                }),
                None => None,
            },
            redirect_to: domain.redirect_to,
            status_code: domain.status_code,
            branch: domain.branch,
            status: domain.status,
            message: domain.message,
            created_at: domain.created_at.timestamp_millis(),
            updated_at: domain.updated_at.timestamp_millis(),
            expiration_time: domain_info
                .as_ref()
                .and_then(|info| info.expiration_time.map(|dt| dt.timestamp_millis())),
            last_renewed: domain_info
                .as_ref()
                .and_then(|info| info.last_renewed.map(|dt| dt.timestamp_millis())),
            service_name: domain.service_name,
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CheckDomainConfigurationRequest {
    pub domain: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CheckDomainConfigurationResponse {
    pub is_configured: bool,
    pub message: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ManualDeploymentQuery {
    pub environment: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateDeploymentSettingsRequest {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDeploymentConfigRequest {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub exposed_port: Option<i32>,
    pub automatic_deploy: Option<bool>,
    pub performance_metrics_enabled: Option<bool>,
    pub session_recording_enabled: Option<bool>,
    pub replicas: Option<i32>,
    pub security: Option<temps_entities::deployment_config::SecurityConfig>,
    /// Build one image per architecture the eligible nodes run. Off by
    /// default; environments inherit this and may override it. Cross-builds
    /// are emulated on the control plane and substantially slower, so they are
    /// opted into rather than triggered by cluster topology.
    pub cross_architecture_builds: Option<bool>,
    /// Project-level default timeout for regular (non-streaming) HTTP
    /// requests, in seconds (0 = no timeout, or 1-86400). Environments may
    /// override this; always clamped to the operator's global hard ceiling
    /// regardless of what's set here. Absent leaves the current value
    /// unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_seconds: Option<i32>,
    /// Project-level default idle timeout for Server-Sent Events streams, in
    /// seconds (0 = no timeout, or 1-86400). Environments may override this;
    /// always clamped to the operator's global hard ceiling. Absent leaves
    /// the current value unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sse_idle_timeout_seconds: Option<i32>,
    /// Project-level default idle timeout for WebSocket connections, in
    /// seconds (0 = no timeout, or 1-86400). Environments may override this;
    /// always clamped to the operator's global hard ceiling. Absent leaves
    /// the current value unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_idle_timeout_seconds: Option<i32>,
    /// Project-level default cap on concurrent in-flight requests to a
    /// single environment's upstream (0 = unlimited). Environments may
    /// override this. Absent leaves the current value unchanged. See
    /// issue #646.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_connections: Option<i32>,
}

/// Deserialize a PATCH integer field while preserving the distinction between
/// an omitted key (`None`) and an explicit JSON null (`Some(None)`).
fn deserialize_optional_optional_i32<'de, D>(
    deserializer: D,
) -> Result<Option<Option<i32>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<i32>::deserialize(deserializer)?))
}

#[derive(Serialize, Deserialize, Clone, ToSchema)]
pub struct UpdateProjectSettingsRequest {
    pub slug: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    pub main_branch: Option<String>,
    pub repo_owner: Option<String>,
    pub repo_name: Option<String>,
    pub preset: Option<String>,
    pub directory: Option<String>,
    /// Enable/disable attack mode (CAPTCHA protection) for all project environments
    pub attack_mode: Option<bool>,
    /// Opt in to AI summarization of metric alert notifications (ADR-021).
    pub ai_alert_summaries_enabled: Option<bool>,
    /// Opt in to AI debugging chat, e.g. on deployment failures (ADR-023).
    pub ai_debug_chat_enabled: Option<bool>,
    /// Opt in to AI propose-then-confirm write capability.
    pub ai_write_actions_enabled: Option<bool>,
    /// Opt in to AI summarization of API traffic analytics.
    pub ai_api_traffic_summary_enabled: Option<bool>,
    /// Opt in to native error-tracking source context (source-file upload +
    /// source code shown in stack traces).
    pub error_source_context_enabled: Option<bool>,
    /// Set the auto-capture source root (relative to the checkout). Send an
    /// empty string to clear it back to the build-context default. Omit to
    /// leave unchanged.
    pub error_source_root: Option<String>,
    /// Enable automatic preview environment creation for each branch
    pub enable_preview_environments: Option<bool>,
    /// When true, newly-created preview environments default to on-demand mode.
    pub preview_envs_on_demand: Option<bool>,
    /// Idle timeout (seconds, 60..=86400) for on-demand preview environments.
    pub preview_envs_idle_timeout_seconds: Option<i32>,
    /// Wake timeout (seconds, 5..=120) for on-demand preview environments.
    pub preview_envs_wake_timeout_seconds: Option<i32>,
    /// How long (hours) to retain built Docker images before nightly cleanup removes them.
    /// Set to null to use the system default. Valid range: 1–8760.
    ///
    /// Omitting the key leaves the current value unchanged; sending an explicit
    /// `null` clears the per-project override. `skip_serializing_if` keeps the
    /// round-trip honest — re-serializing a request that omitted the key must
    /// not emit `"image_retention_hours": null`, which would mean "reset".
    #[serde(
        default,
        deserialize_with = "deserialize_optional_optional_i32",
        skip_serializing_if = "Option::is_none"
    )]
    pub image_retention_hours: Option<Option<i32>>,
    /// Preset-specific configuration (e.g., Dockerfile path for Docker preset)
    ///
    /// Example for Dockerfile preset:
    /// ```json
    /// {
    ///   "dockerfilePath": "docker/Dockerfile",
    ///   "buildContext": "./api"
    /// }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<PresetConfigSchema>)]
    pub preset_config: Option<serde_json::Value>,
    /// ADR-027 Phase 3 opt-out: set to false to suppress this project's traces
    /// from appearing in cross-project discovery results. Default true (consistent
    /// with the OSS global-observability model). Omit to leave unchanged.
    pub cross_project_trace_sharing: Option<bool>,
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

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateGitHubRepoRequest {
    pub repo_owner: Option<String>,
    pub repo_name: Option<String>,
    pub directory: Option<String>,
    pub preset: Option<String>,
    pub main_branch: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, ToSchema)]
pub struct UpdateGitSettingsRequest {
    pub git_provider_connection_id: Option<i32>,
    pub main_branch: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub preset: Option<String>,
    pub directory: String,
    /// Git clone URL for public repositories
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    /// Whether this is a public repository (no git provider connection needed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_public_repo: Option<bool>,
    /// Preset-specific configuration (e.g., Dockerfile path for Docker preset)
    ///
    /// Example for Dockerfile preset:
    /// ```json
    /// {
    ///   "dockerfilePath": "docker/Dockerfile",
    ///   "buildContext": "./api"
    /// }
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<PresetConfigSchema>)]
    pub preset_config: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateAutomaticDeployRequest {
    pub automatic_deploy: bool,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct TemplateEnvVar {
    pub name: String,
    pub example: String,
    pub default: Option<String>,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct Template {
    pub name: String,
    pub github: Option<TemplateGitHub>,
    pub description: Option<String>,
    pub features: Option<Vec<String>>,
    pub services: Option<Vec<String>>,
    pub image: Option<String>,
    pub preset: Option<String>,
    pub env: Option<Vec<TemplateEnvVar>>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TemplateGitHub {
    pub owner: String,
    pub repo: String,
    pub path: Option<String>,
    pub r#ref: String,
}

// Add this new struct with the request schema
#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateProjectFromTemplateRequest {
    pub project_name: String,
    pub github_owner: String,
    pub github_name: String,
    pub template_name: String,
    #[schema(value_type = Option<Vec<ProjectEnvVarInput>>)]
    pub environment_variables: Option<Vec<CreateProjectEnvVar>>,
    pub automatic_deploy: Option<bool>,
    pub performance_metrics_enabled: Option<bool>,
    pub storage_service_ids: Vec<i32>,
}

// Add query parameters struct
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ContainerLogsQuery {
    pub start_date: Option<i64>,
    pub end_date: Option<i64>,
    pub tail: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateCustomDomainRequest {
    pub domain: Option<String>,
    pub environment_id: Option<i32>,
    pub redirect_to: Option<String>,
    pub status_code: Option<i32>,
    pub branch: Option<String>,
    /// Docker Compose service name this domain routes to (empty string clears it)
    pub service_name: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentStateResponse {
    pub id: i32,
    pub state: String,
    pub message: String,
}

// Add this new struct for the request body
#[derive(Serialize, Deserialize, Clone, ToSchema)]
pub struct UpdateEnvironmentSettingsRequest {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub branch: Option<String>,
    pub replicas: Option<i32>,
}

// Add this struct for the response
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectStats {
    pub total_projects: i64,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectStatisticsResponse {
    pub total_count: i64,
}

// Add this struct with the other response types
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectVisitorStats {
    pub visitors_count: i64,
    pub visitors_change: f64,
}

// Add these new structs with the other response types
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectRevenueStats {
    pub revenue_today: f64,
    pub revenue_change: f64,
    pub currency: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ProjectErrorStats {
    pub errors_today: i64,
    pub errors_change: f64,
}

// Add these structs with the other response types
#[derive(Serialize, Deserialize, ToSchema)]
pub struct HourlyVisitorStats {
    pub hourly_visitors: Vec<HourlyCount>,
    pub total_visitors: i64,
    pub total_change: f64,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct HourlyCount {
    pub hour: String,
    pub count: i64,
}

// Add these new response types
#[derive(Serialize, Deserialize, ToSchema)]
pub struct TotalRevenueStats {
    pub total_revenue: f64,
    pub revenue_change: f64,
    pub currency: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TotalVisitorStats {
    pub total_visitors: i64,
    pub total_change: f64,
}

// Add this new struct for the request body
#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateEnvironmentRequest {
    pub name: String,
    pub branch: String,
}

impl From<ProjectError> for Problem {
    fn from(error: ProjectError) -> Self {
        match error {
            ProjectError::DatabaseConnectionError(reason) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(reason)
            }
            ProjectError::PipelineError(reason) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Pipeline Error")
                    .with_detail(reason)
            }
            ProjectError::NotFound(reason) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(format!(
                    "The requested project could not be found: {}",
                    reason
                )),
            ProjectError::GitProviderConnectionNotFound { connection_id } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Git Provider Connection Not Found")
                    .with_detail(format!(
                        "Git provider connection {} not found or not accessible",
                        connection_id
                    ))
            }

            ProjectError::TemplateNotFound => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Template Not Found")
                .with_detail("The requested template could not be found"),

            ProjectError::DatabaseError { reason } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(reason)
            }

            ProjectError::SlugAlreadyExists(slug) => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Slug Already Exists")
                .with_detail(format!("A project with slug '{}' already exists", slug)),

            ProjectError::SlugConflict { slug } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Slug Conflict")
                .with_detail(format!(
                    "A project with slug '{}' was created concurrently. Please retry.",
                    slug
                )),

            err @ (ProjectError::EnvironmentCreationFailed { .. }
            | ProjectError::EnvVarCreationFailed { .. }
            | ProjectError::StorageLinkFailed { .. }) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Project Creation Failed")
                    .with_detail(err.to_string())
            }

            ProjectError::InvalidInput(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Input")
                .with_detail(msg),

            ProjectError::GitHubError(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("GitHub Error")
                .with_detail(msg),

            ProjectError::DeploymentError(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Deployment Error")
                    .with_detail(msg)
            }

            ProjectError::DeploymentCleanupFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Project Runtime Cleanup Failed")
                    .with_detail(error.to_string())
            }

            ProjectError::RouteReloadFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Proxy Route Reload Failed")
                    .with_detail(error.to_string())
            }

            ProjectError::Other(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(msg),

            ProjectError::InvalidGitUrl { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Git URL")
                .with_detail(error.to_string()),
        }
    }
}

// Custom Domain Error conversions
impl From<crate::services::custom_domains::CustomDomainError> for Problem {
    fn from(error: crate::services::custom_domains::CustomDomainError) -> Self {
        use crate::services::custom_domains::CustomDomainError;

        match error {
            CustomDomainError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail("A database operation failed while managing custom domains")
            }
            CustomDomainError::NotFound(msg) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Custom Domain Not Found")
                .with_detail(msg),
            CustomDomainError::InvalidDomain(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Domain")
                .with_detail(msg),
            CustomDomainError::DuplicateDomain(msg) => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Duplicate Domain")
                .with_detail(msg),
            CustomDomainError::CircularRedirect(msg) => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Circular Redirect")
                    .with_detail(msg)
            }
            CustomDomainError::InvalidRedirectUrl(msg) => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Redirect URL")
                    .with_detail(msg)
            }
            CustomDomainError::AssignmentChanged {
                domain_id,
                source_project_id,
            } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Domain Assignment Changed")
                .with_detail(format!(
                    "Custom domain {domain_id} is no longer assigned to source project {source_project_id}; refresh and try again"
                )),
            CustomDomainError::AuditIntentFailed {
                domain_id,
                source_project_id,
                target_project_id,
                ..
            } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Domain Reassignment Audit Failed")
                    .with_detail(format!(
                        "Custom domain {domain_id} could not be reassigned from project {source_project_id} to project {target_project_id} because the required audit record could not be persisted; no ownership change was made"
                    ))
            }
            CustomDomainError::EnrichmentDatabase {
                operation,
                domain_id,
                certificate_id,
                environment_id,
                ..
            } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Custom Domain Metadata Unavailable")
                .with_detail(format!(
                    "Could not {operation} for custom domain {domain_id} (certificate {certificate_id:?}, environment {environment_id})"
                )),
            CustomDomainError::ReassignmentDatabase {
                operation,
                domain_id,
                source_project_id,
                target_project_id,
                target_environment_id,
                ..
            } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Domain Reassignment Failed")
                .with_detail(format!(
                    "Database operation '{operation}' failed while reassigning custom domain {domain_id} from project {source_project_id} to project {target_project_id} environment {target_environment_id}"
                )),
            CustomDomainError::Internal(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(msg)
            }
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ListCustomDomainsResponse {
    pub domains: Vec<CustomDomainResponse>,
    pub total: usize,
}

// Preset-related types
#[derive(Serialize, Deserialize, ToSchema)]
pub struct PresetResponse {
    /// Unique identifier slug for the preset
    pub slug: String,
    /// Display name/label for the preset
    pub label: String,
    /// Icon URL for the preset
    pub icon_url: String,
    /// Project type (server or static)
    pub project_type: String,
    /// Description of what this preset does
    pub description: String,
    /// Default port the application listens on (None for static sites)
    #[schema(example = 3000)]
    pub default_port: Option<u16>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ListPresetsResponse {
    pub presets: Vec<PresetResponse>,
    pub total: usize,
}

/// Request body for generating a Dockerfile from a preset
#[derive(Deserialize, ToSchema)]
pub struct GenerateDockerfileRequest {
    /// Package manager used by the project (npm, yarn, pnpm, bun)
    /// If not provided, defaults to npm
    #[schema(example = "npm")]
    pub package_manager: Option<String>,
    /// Custom install command (overrides preset default)
    #[schema(example = "npm ci")]
    pub install_command: Option<String>,
    /// Custom build command (overrides preset default)
    #[schema(example = "npm run build")]
    pub build_command: Option<String>,
    /// Output directory for static builds
    #[schema(example = "dist")]
    pub output_dir: Option<String>,
    /// Project name/slug used for container naming
    #[schema(example = "my-app")]
    pub project_name: Option<String>,
    /// Whether to use BuildKit cache mounts for faster builds
    #[serde(default = "default_true")]
    pub use_buildkit: bool,
}

fn default_true() -> bool {
    true
}

/// Response containing a generated Dockerfile and build arguments
#[derive(Serialize, ToSchema)]
pub struct GenerateDockerfileResponse {
    /// The generated Dockerfile content
    pub dockerfile: String,
    /// Build arguments to pass to `docker build --build-arg KEY=VALUE`
    pub build_args: std::collections::HashMap<String, String>,
    /// The preset slug used for generation
    pub preset: String,
}

/// Response for `POST /projects/{project_id}/gitlab/reinstall-webhook`
#[derive(Serialize, ToSchema)]
pub struct ReinstallWebhookResponse {
    /// The new GitLab hook ID that was installed.
    pub hook_id: i32,
    /// Human-readable status message.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::custom_domains::CustomDomainError;
    use axum::response::IntoResponse;

    #[test]
    fn test_custom_domain_error_assignment_changed_maps_to_conflict_with_context() {
        let problem: Problem = CustomDomainError::AssignmentChanged {
            domain_id: 41,
            source_project_id: 7,
        }
        .into();

        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert_eq!(
            problem.body.get("title"),
            Some(&serde_json::json!("Domain Assignment Changed"))
        );
        assert_eq!(
            problem.body.get("detail"),
            Some(&serde_json::json!(
                "Custom domain 41 is no longer assigned to source project 7; refresh and try again"
            ))
        );
        assert_eq!(problem.into_response().status(), StatusCode::CONFLICT);
    }

    #[test]
    fn image_retention_patch_distinguishes_omitted_null_and_value() {
        let omitted: UpdateProjectSettingsRequest = serde_json::from_str("{}").unwrap();
        let cleared: UpdateProjectSettingsRequest =
            serde_json::from_str(r#"{"image_retention_hours":null}"#).unwrap();
        let set: UpdateProjectSettingsRequest =
            serde_json::from_str(r#"{"image_retention_hours":72}"#).unwrap();

        assert_eq!(omitted.image_retention_hours, None);
        assert_eq!(cleared.image_retention_hours, Some(None));
        assert_eq!(set.image_retention_hours, Some(Some(72)));
    }
}
