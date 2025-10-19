
use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
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
    pub started_at: Option<UtcDateTime>,
    pub finished_at: Option<UtcDateTime>,
    pub screenshot_location: Option<String>,
    pub commit_author: Option<String>,
    pub commit_date: Option<UtcDateTime>,
    pub is_current: bool,
    pub cancelled_reason: Option<String>,
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


#[derive(Deserialize)]
pub struct UpdateDeploymentSettingsRequest {
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
}


#[derive(Debug, Serialize)]
pub struct Pipeline {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: i32,
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
    pub commit_json: Option<serde_json::Value>,
}
