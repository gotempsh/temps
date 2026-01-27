use std::sync::Arc;

use crate::services::database_cron_service::DatabaseCronConfigService;
use crate::services::remote_deployment_service::RemoteDeploymentService;
use crate::services::workflow_planner::WorkflowPlanner;
use crate::services::ExternalDeploymentManager;
use crate::services::WorkflowExecutionService;
use crate::DeploymentService;
use sea_orm::DatabaseConnection;

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
}

fn default_timestamps() -> bool {
    false
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
}

impl From<temps_deployer::ContainerInfo> for ContainerInfoResponse {
    fn from(info: temps_deployer::ContainerInfo) -> Self {
        Self {
            container_id: info.container_id,
            container_name: info.container_name,
            image_name: info.image_name,
            status: info.status.to_string(),
            created_at: info.created_at.to_rfc3339(),
        }
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
    /// Resource limits
    #[schema(nullable = true)]
    pub resource_limits: Option<ResourceLimitsResponse>,
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
    /// CPU usage percentage (0-100)
    pub cpu_percent: f64,
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
