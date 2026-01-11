use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
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
    pub enable_preview_environments: bool,
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
    pub environment_variables: Option<Vec<(String, String)>>,
    pub automatic_deploy: bool,
    pub storage_service_ids: Vec<i32>,
    pub is_public_repo: Option<bool>,
    pub git_url: Option<String>,
    pub git_provider_connection_id: Option<i32>,
    pub exposed_port: Option<i32>,
}

#[derive(Deserialize)]
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

    #[error("Template not found")]
    TemplateNotFound,

    #[error("Database error: {reason}")]
    DatabaseError { reason: String },

    #[error("Slug already exists: {0}")]
    SlugAlreadyExists(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("GitHub error: {0}")]
    GitHubError(String),

    #[error("Deployment error: {0}")]
    DeploymentError(String),

    #[error("Other error: {0}")]
    Other(String),

    #[error("Pipeline error: {0}")]
    PipelineError(String),
}

impl From<sea_orm::DbErr> for ProjectError {
    fn from(error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::RecordNotFound(_) => ProjectError::NotFound(error.to_string()),
            sea_orm::DbErr::Exec(ref err) if err.to_string().contains("UNIQUE") => {
                ProjectError::DatabaseError {
                    reason: "A unique constraint was violated".to_string(),
                }
            }
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
