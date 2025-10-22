use crate::services::{
    cache::GitProviderCacheManager, git_provider_manager::GitProviderManager,
    github::GithubAppService, repository::RepositoryService,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::AuditLogger;
use utoipa::ToSchema;

pub struct GitAppState {
    pub git_provider_manager: Arc<GitProviderManager>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub repository_service: Arc<RepositoryService>,
    // This should be removed, all functionality should be handled by the git_provider_manager
    pub github_service: Arc<GithubAppService>,
    pub config_service: Arc<ConfigService>,
    pub cache_manager: Arc<GitProviderCacheManager>,
}

pub fn create_git_app_state(
    repository_service: Arc<RepositoryService>,
    git_provider_manager: Arc<GitProviderManager>,
    config_service: Arc<ConfigService>,
    audit_service: Arc<dyn AuditLogger>,
    github_service: Arc<GithubAppService>,
    cache_manager: Arc<GitProviderCacheManager>,
) -> Arc<GitAppState> {
    Arc::new(GitAppState {
        git_provider_manager,
        audit_service,
        repository_service,
        github_service,
        config_service,
        cache_manager,
    })
}

#[derive(Serialize, Deserialize, ToSchema, PartialEq)]
pub struct GithubAppResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub app_id: Option<i32>,
    pub client_id: String,
    pub url: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct SetupStatusResponse {
    pub is_setup: bool,
    pub message: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct FrameworkResponse {
    framework: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PaginationParams {
    #[schema(default = 30, minimum = 1, maximum = 100)]
    pub per_page: Option<u8>,
    #[schema(default = 1, minimum = 1)]
    pub page: Option<u32>,
    #[schema(default = "")]
    pub search_term: String,
    pub installation_id: i32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GithubInstallationResponse {
    pub id: i32,
    pub github_app_id: i32,
    pub installation_id: i32,
    pub account_id: i32,
    pub account_name: String,
    pub account_type: String,
    pub html_url: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub suspended_at: Option<i64>,
    pub suspended_by: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct RepoInfoResponse {
    pub presets: Vec<Preset>,
}

#[derive(Serialize, Deserialize, Debug, ToSchema)]
pub struct Preset {
    pub label: String,
    pub project_type: String,
    pub slug: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct RepoSourceResponse {
    pub id: String,
    pub name: String,
    pub source_type: RepoSourceType,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RepoSourceType {
    Installation,
    Auth,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct BranchResponse {
    pub name: String,
    pub commit_sha: String,
    pub protected: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct BranchListResponse {
    pub branches: Vec<BranchResponse>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TagResponse {
    pub name: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TagListResponse {
    pub tags: Vec<TagResponse>,
}
