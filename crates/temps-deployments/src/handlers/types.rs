use std::sync::Arc;

use crate::services::database_cron_service::DatabaseCronConfigService;
use crate::services::node_service::NodeService;
use crate::services::remote_deployment_service::RemoteDeploymentService;
use crate::services::workflow_planner::WorkflowPlanner;
use crate::services::ExternalDeploymentManager;
use crate::services::WorkflowExecutionService;
use crate::DeploymentService;
use sea_orm::DatabaseConnection;
use temps_core::AuditLogger;

pub struct AppState {
    pub deployment_service: Arc<DeploymentService>,
    pub log_service: Arc<temps_logs::LogService>,
    pub cron_service: Arc<DatabaseCronConfigService>,
    pub external_deployment_manager: Arc<ExternalDeploymentManager>,
    pub remote_deployment_service: Arc<RemoteDeploymentService>,
    // Services for remote deployments
    pub db: Arc<DatabaseConnection>,
    pub workflow_planner: Arc<WorkflowPlanner>,
    pub workflow_executor: Arc<WorkflowExecutionService>,
    pub queue_service: Arc<dyn temps_core::JobQueue>,
    // Blob service for static bundle uploads (optional, falls back to local storage)
    pub blob_service: Arc<temps_blob::BlobService>,
    /// Data directory for local file storage (static bundles, etc.)
    pub data_dir: std::path::PathBuf,
    /// Image builder for importing Docker images from tarballs
    pub image_builder: Arc<dyn temps_deployer::ImageBuilder>,
    /// Audit logging service
    pub audit_service: Arc<dyn AuditLogger>,
    /// Node service for listing/getting worker nodes (UI-facing)
    pub node_service: Arc<NodeService>,
    /// Encryption service for decrypting node tokens (used by drain to stop remote containers)
    pub encryption_service: Arc<temps_core::EncryptionService>,
    /// Config service — gives drain/exit-facing handlers access to the cluster
    /// CA so CP→agent calls to `https://` nodes use mutual TLS (ADR-020 WS-2.1)
    pub config_service: Arc<temps_config::ConfigService>,
    /// Docker client for container exec/terminal
    pub docker: Arc<bollard::Docker>,
    /// Optional gate checked before manual-deploy handlers transition a
    /// deployment to `Running` (e.g. a plugin implementing manual
    /// approvals). `None` when no such plugin is registered — deploys
    /// proceed unconditionally, matching today's behaviour. Safe to
    /// resolve once here (unlike the job processor's `DeploymentGateSlot`):
    /// `configure_routes` runs only after every plugin's
    /// `initialize_plugin_services` has completed, so a registered gate is
    /// guaranteed to already be present by this point. See
    /// [`temps_core::DeploymentGate`].
    pub deployment_gate: Option<Arc<dyn temps_core::DeploymentGate>>,
    /// Optional checker enforcing team-based project access for human sessions.
    ///
    /// `None` when no plugin implementing this check is registered — the
    /// `project_access_guard!` macro is a strict synchronous no-op in that
    /// case. Resolved once in `configure_routes` via
    /// `context.get_service::<dyn temps_core::ProjectAccessChecker>()`.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// Resolves the per-managed-domain public hostname strategy (Standard/Flat).
    pub hostname_resolver: Arc<dyn temps_core::PublicHostnameResolver>,
}

use crate::services::types::Deployment;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
pub struct GetDeploymentsParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub environment_id: Option<i32>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentDomainResponse {
    pub id: i32,
    pub domain: String,
}

/// Metadata for one captured (historical) container-log dump. Listed on the
/// deployment detail page so a user can pick which past container's logs to read.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentContainerLogResponse {
    pub id: i32,
    pub deployment_id: i32,
    pub container_id: String,
    pub container_name: String,
    pub service_name: Option<String>,
    pub node_id: Option<i32>,
    pub size_bytes: i64,
    pub truncated: bool,
    /// Unix epoch milliseconds of when the logs were captured (just before
    /// teardown). Matches the timestamp convention used by `DeploymentResponse`.
    pub captured_at: i64,
}

impl From<temps_entities::deployment_container_logs::Model> for DeploymentContainerLogResponse {
    fn from(m: temps_entities::deployment_container_logs::Model) -> Self {
        Self {
            id: m.id,
            deployment_id: m.deployment_id,
            container_id: m.container_id,
            container_name: m.container_name,
            service_name: m.service_name,
            node_id: m.node_id,
            size_bytes: m.size_bytes,
            truncated: m.truncated,
            captured_at: m.captured_at.timestamp_millis(),
        }
    }
}

/// The list of captured container-log dumps for a deployment.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentContainerLogsListResponse {
    pub logs: Vec<DeploymentContainerLogResponse>,
}

/// A single captured container-log dump, including its full text content.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentContainerLogContentResponse {
    pub id: i32,
    pub container_name: String,
    pub service_name: Option<String>,
    pub size_bytes: i64,
    pub truncated: bool,
    pub captured_at: i64,
    /// The captured plain-text log content.
    pub content: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateProjectRequest {
    pub name: String,
    pub repo_name: Option<String>,
    pub repo_owner: Option<String>,
    pub directory: String,
    pub main_branch: String,
    pub preset: String,
    pub output_dir: Option<String>,
    pub build_command: Option<String>,
    pub install_command: Option<String>,
    pub environment_variables: Option<Vec<(String, String)>>,
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

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentDomains {
    pub domains: Vec<String>,
    pub environment_id: i32,
    pub environment_slug: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TriggerPipelinePayload {
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub commit: Option<String>,
    pub environment_id: i32,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentListResponse {
    pub deployments: Vec<DeploymentResponse>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment: DeploymentEnvironmentResponse,
    pub status: String,
    pub url: String,
    pub commit_hash: Option<String>,
    pub commit_message: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub screenshot_location: Option<String>,
    pub commit_author: Option<String>,
    pub commit_date: Option<i64>,
    pub is_current: bool,
    pub cancelled_reason: Option<String>,
    /// Deployment configuration snapshot (CPU, memory, replicas, environment variables, etc.)
    pub deployment_config: Option<temps_entities::deployment_config::DeploymentConfigSnapshot>,
    /// Deployment metadata (build info, git event, etc.)
    pub metadata: Option<temps_entities::prelude::DeploymentMetadata>,
}

// Add new struct for environment info in response
#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentEnvironmentResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub domains: Vec<String>,
}

impl DeploymentResponse {
    pub fn from_service_deployment(deployment: Deployment) -> Self {
        Self {
            id: deployment.id,
            project_id: deployment.project_id,
            environment_id: deployment.environment_id,
            environment: DeploymentEnvironmentResponse {
                id: deployment.environment.id,
                name: deployment.environment.name,
                slug: deployment.environment.slug,
                domains: deployment.environment.domains,
            },
            status: deployment.status,
            url: deployment.url,
            commit_hash: deployment.commit_hash,
            commit_message: deployment.commit_message,
            branch: deployment.branch,
            tag: deployment.tag,
            created_at: deployment.created_at.timestamp_millis(),
            started_at: deployment.started_at.map(|d| d.timestamp_millis()),
            finished_at: deployment.finished_at.map(|d| d.timestamp_millis()),
            screenshot_location: deployment.screenshot_location,
            commit_author: deployment.commit_author,
            commit_date: deployment.commit_date.map(|d| d.timestamp_millis()),
            is_current: deployment.is_current,
            cancelled_reason: deployment.cancelled_reason,
            deployment_config: deployment.deployment_config,
            metadata: deployment.metadata,
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
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DomainEnvironmentResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
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
pub struct UpdateProjectSettingsRequest {
    pub slug: Option<String>,
    pub git_provider_connection_id: Option<i32>,
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
            cpu_request: env.deployment_config.as_ref().and_then(|c| c.cpu_request),
            cpu_limit: env.deployment_config.as_ref().and_then(|c| c.cpu_limit),
            memory_request: env
                .deployment_config
                .as_ref()
                .and_then(|c| c.memory_request),
            memory_limit: env.deployment_config.as_ref().and_then(|c| c.memory_limit),
            replicas: env.deployment_config.as_ref().map(|c| c.replicas),
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

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateGitHubRepoRequest {
    pub repo_owner: Option<String>,
    pub repo_name: Option<String>,
    pub directory: Option<String>,
    pub preset: Option<String>,
    pub main_branch: Option<String>,
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
    pub environment_variables: Option<Vec<(String, String)>>,
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
    /// Optional container name to get logs from (if deployment has multiple containers)
    pub container_name: Option<String>,
    /// Include timestamps in log output (default: false)
    #[serde(default = "default_timestamps")]
    pub timestamps: bool,
    /// Follow log output in real-time (default: true for backward compatibility)
    #[serde(default = "default_follow")]
    pub follow: bool,
}

fn default_timestamps() -> bool {
    false
}

fn default_follow() -> bool {
    true
}

#[derive(Deserialize, ToSchema)]
pub struct JobLogsQuery {
    pub lines: Option<usize>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct Pipeline {
    pub id: i32,
    pub project_id: i32,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub commit_hash: Option<String>,
    pub commit_message: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
}

impl From<crate::services::types::Pipeline> for Pipeline {
    fn from(pipeline: crate::services::types::Pipeline) -> Self {
        Self {
            id: pipeline.id,
            project_id: pipeline.project_id,
            status: pipeline.status,
            created_at: pipeline.created_at.timestamp_millis(),
            updated_at: pipeline.updated_at.timestamp_millis(),
            commit_hash: pipeline.commit_sha,
            commit_message: pipeline.commit_message,
            branch: pipeline.branch_ref,
            tag: pipeline.tag_ref,
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateCustomDomainRequest {
    pub redirect_to: Option<String>,
    pub status_code: Option<i32>,
    pub branch: Option<String>,
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

#[derive(Deserialize, ToSchema)]
pub struct PromoteDeploymentRequest {
    /// Target environment ID to promote the deployment to
    pub target_environment_id: i32,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentJobResponse {
    pub id: i32,
    pub deployment_id: i32,
    pub job_id: String,
    pub job_type: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub log_id: String,
    pub error_message: Option<String>,
    pub job_config: Option<serde_json::Value>,
    pub outputs: Option<serde_json::Value>,
    pub dependencies: Option<serde_json::Value>,
    pub execution_order: Option<i32>,
}

impl From<temps_entities::deployment_jobs::Model> for DeploymentJobResponse {
    fn from(job: temps_entities::deployment_jobs::Model) -> Self {
        Self {
            id: job.id,
            deployment_id: job.deployment_id,
            job_id: job.job_id,
            job_type: job.job_type,
            name: job.name,
            description: job.description,
            status: job.status.to_string(),
            created_at: job.created_at.timestamp_millis(),
            updated_at: job.updated_at.timestamp_millis(),
            started_at: job.started_at.map(|t| t.timestamp_millis()),
            finished_at: job.finished_at.map(|t| t.timestamp_millis()),
            log_id: job.log_id,
            error_message: job.error_message,
            job_config: job.job_config,
            outputs: job.outputs,
            dependencies: job.dependencies,
            execution_order: job.execution_order,
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeploymentJobsResponse {
    pub jobs: Vec<DeploymentJobResponse>,
    pub total: usize,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ContainerInfoResponse {
    pub container_id: String,
    pub container_name: String,
    pub image_name: String,
    pub status: String,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
    /// Node name where this container is running. None for local (single-node) deployments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    /// Compose service name (e.g. "web", "redis"). None for single-container deployments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Per-service URL for compose deployments (e.g. "https://web-myapp.localho.st")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_url: Option<String>,
    /// Process exit code reported by Docker. None while still running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Human-readable reason the container exited (e.g. "OOMKilled",
    /// "Killed by SIGKILL (exit code 137)", "Exit code 1"). None while running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_reason: Option<String>,
    /// True when Docker's OOM killer terminated the container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oom_killed: Option<bool>,
    /// Free-form error string from Docker's container state on exit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// When the container exited (Docker's FinishedAt). None while running.
    #[schema(example = "2025-10-12T12:16:47.609192Z")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// When the container's main process most recently started. The UI uses
    /// this for the uptime label so the count resets when a container is
    /// restarted in place. None for containers that never started.
    #[schema(example = "2025-10-12T12:15:50.000000Z")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Container restart count from Docker. The UI shows a chip when this is
    /// > 0 so a crash loop is visible without opening detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_count: Option<i64>,
    /// CPU limit in whole cores (e.g. 1.0). None when no limit is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit_cores: Option<f64>,
}

impl ContainerInfoResponse {
    pub fn from_info(
        info: temps_deployer::ContainerInfo,
        node_name: Option<String>,
        service_name: Option<String>,
        service_url: Option<String>,
    ) -> Self {
        Self {
            container_id: info.container_id,
            container_name: info.container_name,
            image_name: info.image_name,
            status: info.status.to_string(),
            created_at: info.created_at.to_rfc3339(),
            node_name,
            service_name,
            service_url,
            exit_code: info.exit_code,
            exit_reason: info.exit_reason,
            oom_killed: info.oom_killed,
            error_message: info.error_message,
            finished_at: info.finished_at.map(|d| d.to_rfc3339()),
            started_at: info.started_at.map(|d| d.to_rfc3339()),
            restart_count: info.restart_count,
            cpu_limit_cores: info.cpu_limit_cores,
        }
    }
}

impl From<temps_deployer::ContainerInfo> for ContainerInfoResponse {
    fn from(info: temps_deployer::ContainerInfo) -> Self {
        Self::from_info(info, None, None, None)
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ContainerListResponse {
    pub containers: Vec<ContainerInfoResponse>,
    pub total: usize,
}

/// Detailed container information with environment variables and metrics
#[derive(Serialize, ToSchema)]
pub struct ContainerDetailResponse {
    pub id: i32,
    pub container_id: String,
    pub container_name: String,
    pub image_name: String,
    pub status: String,
    pub deployment_id: i32,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub deployed_at: String,
    #[schema(nullable = true, example = "2025-10-12T12:16:47.609192Z")]
    pub ready_at: Option<String>,
    /// Port inside the container
    pub container_port: i32,
    /// Port on the host machine
    #[schema(nullable = true)]
    pub host_port: Option<i32>,
    /// Environment variables (sensitive values masked)
    pub environment_variables: Vec<EnvVarResponse>,
    /// Container restart count from Docker
    #[schema(nullable = true)]
    pub restart_count: Option<i64>,
    /// Resource limits
    #[schema(nullable = true)]
    pub resource_limits: Option<ResourceLimitsResponse>,
    /// Compose service name (e.g. "web", "redis"). None for single-container deployments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Per-service URL for compose deployments
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_url: Option<String>,
    /// Process exit code reported by Docker. None while still running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Human-readable reason the container exited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_reason: Option<String>,
    /// True when Docker's OOM killer terminated the container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oom_killed: Option<bool>,
    /// Free-form error string from Docker's container state on exit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// When the container exited (Docker's FinishedAt). None while running.
    #[schema(example = "2025-10-12T12:16:47.609192Z")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// When the container's main process most recently started.
    #[schema(example = "2025-10-12T12:15:50.000000Z")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// CPU limit in whole cores (e.g. 1.0). None when no limit is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_limit_cores: Option<f64>,
}

/// Environment variable with masked sensitive values
#[derive(Serialize, ToSchema)]
pub struct EnvVarResponse {
    pub key: String,
    pub value: String,
    /// Whether this is a sensitive/masked value
    pub is_masked: bool,
}

/// Container resource limits
#[derive(Serialize, ToSchema)]
pub struct ResourceLimitsResponse {
    #[schema(nullable = true)]
    pub cpu_request: Option<i32>,
    #[schema(nullable = true)]
    pub cpu_limit: Option<i32>,
    #[schema(nullable = true)]
    pub memory_request: Option<i32>,
    #[schema(nullable = true)]
    pub memory_limit: Option<i32>,
}

/// Container resource metrics (CPU, memory usage)
#[derive(Serialize, ToSchema)]
pub struct ContainerMetricsResponse {
    pub container_id: String,
    pub container_name: String,
    /// CPU usage as a multi-core percentage (Docker convention: 200 = 2 cores
    /// fully pinned). Divide by 100 to get cores used.
    pub cpu_percent: f64,
    /// CPU limit in whole cores (e.g. 1.0). None = no limit.
    #[schema(nullable = true)]
    pub cpu_limit_cores: Option<f64>,
    /// Memory usage in bytes
    pub memory_bytes: u64,
    /// Memory limit in bytes (if set)
    #[schema(nullable = true)]
    pub memory_limit_bytes: Option<u64>,
    /// Memory usage percentage (0-100) if limit is set
    #[schema(nullable = true)]
    pub memory_percent: Option<f64>,
    /// Network bytes received
    pub network_rx_bytes: u64,
    /// Network bytes transmitted
    pub network_tx_bytes: u64,
    /// Timestamp of metrics collection
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub timestamp: String,
}

/// Response indicating success of container state change
#[derive(Serialize, ToSchema)]
pub struct ContainerActionResponse {
    pub container_id: String,
    pub container_name: String,
    pub action: String,
    pub status: String,
    pub message: String,
}

/// Query parameters for activity graph endpoint
#[derive(Deserialize, ToSchema)]
pub struct ActivityGraphQuery {
    /// Optional project ID to filter activity
    pub project_id: Option<i32>,
    /// Optional environment ID to filter activity
    pub environment_id: Option<i32>,
    /// Number of days to include (default: 365 for last year)
    #[serde(default = "default_days")]
    pub days: i32,
}

fn default_days() -> i32 {
    365
}

/// Response for activity graph showing daily deployment activity
#[derive(Serialize, ToSchema)]
pub struct ActivityGraphResponse {
    /// Array of daily activity counts
    pub days: Vec<ActivityDay>,
    /// Total count of activities across all days
    pub total_count: i64,
    /// Date range start (YYYY-MM-DD)
    #[schema(example = "2024-01-01")]
    pub start_date: String,
    /// Date range end (YYYY-MM-DD)
    #[schema(example = "2024-12-31")]
    pub end_date: String,
}

/// Daily activity count for a single day
#[derive(Serialize, ToSchema)]
pub struct ActivityDay {
    /// Date in YYYY-MM-DD format
    #[schema(example = "2024-06-15")]
    pub date: String,
    /// Number of deployments on this day
    pub count: i64,
    /// Intensity level (0-4) for visualization
    /// 0: No activity, 1: Low (1-2), 2: Medium (3-5), 3: High (6-10), 4: Very High (11+)
    #[schema(example = 2)]
    pub level: i32,
}
