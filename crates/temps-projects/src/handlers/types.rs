use serde::{Deserialize, Serialize};
use temps_core::templates::TemplateService;
use temps_core::UtcDateTime;
use temps_entities::deployment_config::DeploymentConfig;
use utoipa::ToSchema;

use crate::services::custom_domains::CustomDomainService;
use crate::services::project::ProjectService;
use crate::services::types::ProjectError;
use http::StatusCode;
use std::sync::Arc;
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;
use temps_core::AuditLogger;
use temps_presets::preset_config_schema::PresetConfigSchema;

pub struct AppState {
    pub project_service: Arc<ProjectService>,
    pub custom_domain_service: Arc<CustomDomainService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub template_service: Arc<TemplateService>,
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
    /// - **Nixpacks preset**: Uses `nixpacks.toml` file for configuration (no params needed)
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
    pub created_at: i64,
    pub updated_at: i64,
    pub last_deployment: Option<i64>,
    pub git_provider_connection_id: Option<i32>,
    /// Deployment configuration (resources, autoscaling, features)
    pub deployment_config: DeploymentConfig,
    /// Attack mode - when enabled, requires CAPTCHA verification for all project environments
    pub attack_mode: bool,
    /// Enable automatic preview environment creation for each branch
    pub enable_preview_environments: bool,
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
            created_at: project.created_at.timestamp_millis(),
            updated_at: project.updated_at.timestamp_millis(),
            last_deployment: project.last_deployment.map(|d| d.timestamp_millis()),
            git_provider_connection_id: project.git_provider_connection_id,
            attack_mode: project.attack_mode,
            enable_preview_environments: project.enable_preview_environments,
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
                    .map(|c| c.automatic_deploy)
                    .unwrap_or(false),
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
    /// Enable automatic preview environment creation for each branch
    pub enable_preview_environments: Option<bool>,
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
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpdateCustomDomainRequest {
    pub domain: Option<String>,
    pub environment_id: Option<i32>,
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

            ProjectError::TemplateNotFound => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Template Not Found")
                .with_detail("The requested template could not be found"),

            ProjectError::DatabaseError { reason } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(reason)
            }

            ProjectError::SlugAlreadyExists(slug) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Slug Already Exists")
                .with_detail(format!("A project with slug '{}' already exists", slug)),

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

            ProjectError::Other(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(msg),
        }
    }
}

// Custom Domain Error conversions
impl From<crate::services::custom_domains::CustomDomainError> for Problem {
    fn from(error: crate::services::custom_domains::CustomDomainError) -> Self {
        use crate::services::custom_domains::CustomDomainError;

        match error {
            CustomDomainError::Database(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(msg.to_string())
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
