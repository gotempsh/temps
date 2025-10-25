use super::repositories::{
    check_commit_exists, get_branches_by_repository_id, get_repository_branches,
    get_repository_tags, get_tags_by_repository_id,
};
use super::types::GitAppState as AppState;
use super::types::GitAppState;
use crate::services::git_provider::GitProviderError;
use crate::services::git_provider_manager::GitProviderManagerError;
use crate::services::repository::RepositoryServiceError;
use crate::services::{
    git_provider::{AuthMethod, GitProviderType, RepositoryListParams},
    repository::RepositoryFilter,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_auth::{permission_check, Permission, RequireAuth};

use temps_core::problemdetails::{new as problem_new, Problem};
use temps_core::UtcDateTime;
use utoipa::ToSchema;

// Convert RepositoryServiceError to Problem Details
impl From<RepositoryServiceError> for Problem {
    fn from(error: RepositoryServiceError) -> Self {
        match error {
            RepositoryServiceError::DatabaseError(e) => {
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(e.to_string())
            }
            RepositoryServiceError::ConnectionNotFound => problem_new(StatusCode::NOT_FOUND)
                .with_title("Connection Not Found")
                .with_detail("The specified git provider connection was not found"),
        }
    }
}

// Convert GitProviderManagerError to Problem Details
impl From<GitProviderManagerError> for Problem {
    fn from(error: GitProviderManagerError) -> Self {
        match error {
            GitProviderManagerError::DatabaseError(e) => {
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(e.to_string())
            }
            GitProviderManagerError::ProviderError(e) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("Provider Error")
                .with_detail(e.to_string()),
            GitProviderManagerError::ProviderNotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_title("Provider Not Found")
                .with_detail(msg),
            GitProviderManagerError::ConnectionNotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_title("Connection Not Found")
                .with_detail(msg),
            GitProviderManagerError::InvalidConfiguration(msg) => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Configuration")
                    .with_detail(msg)
            }
            GitProviderManagerError::JsonError(e) => problem_new(StatusCode::BAD_REQUEST)
                .with_title("JSON Error")
                .with_detail(e.to_string()),
            GitProviderManagerError::SyncInProgress => problem_new(StatusCode::CONFLICT)
                .with_title("Sync Already In Progress")
                .with_detail(
                    "Repository synchronization is already in progress for this connection",
                ),
            GitProviderManagerError::RepositoryNotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_title("Repository Not Found")
                .with_detail(msg),
            GitProviderManagerError::ConnectionTokenExpired { connection_id } => {
                problem_new(StatusCode::BAD_REQUEST)
                    .with_title("Connection Token Expired")
                    .with_detail(format!(
                        "The access token for connection {} has expired or is invalid. Please update your access token using the /git-connections/{}/update-token endpoint.",
                        connection_id, connection_id
                    ))
                    .with_type("https://docs.temps.sh/errors/expired_token")
                    .with_value("code", "expired_token")
                    .with_value("connection_id", connection_id)
            }
            GitProviderManagerError::QueueError(msg) => problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Queue Error")
                .with_detail(msg),
        }
    }
}

impl From<GitProviderError> for Problem {
    fn from(error: GitProviderError) -> Self {
        match error {
            GitProviderError::DatabaseError(e) => problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_type("https://docs.temps.sh/errors/database_error")
                .with_title("Database Error")
                .with_detail(e.to_string()),
            GitProviderError::ProviderNotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_type("https://docs.temps.sh/errors/provider_not_found")
                .with_title("Provider Not Found")
                .with_detail(msg),
            GitProviderError::ConnectionNotFound(msg) => problem_new(StatusCode::NOT_FOUND)
                .with_type("https://docs.temps.sh/errors/connection_not_found")
                .with_title("Connection Not Found")
                .with_detail(msg),
            GitProviderError::AuthenticationFailed(msg) => problem_new(StatusCode::UNAUTHORIZED)
                .with_type("https://docs.temps.sh/errors/authentication_failed")
                .with_title("Authentication Failed")
                .with_detail(msg),
            GitProviderError::ApiError(msg) => problem_new(StatusCode::BAD_GATEWAY)
                .with_type("https://docs.temps.sh/errors/api_error")
                .with_title("API Error")
                .with_detail(msg),
            GitProviderError::NotImplemented => problem_new(StatusCode::NOT_IMPLEMENTED)
                .with_type("https://docs.temps.sh/errors/not_implemented")
                .with_title("Not Implemented")
                .with_detail("This operation is not implemented for this provider"),
            GitProviderError::InvalidConfiguration(msg) => problem_new(StatusCode::BAD_REQUEST)
                .with_type("https://docs.temps.sh/errors/invalid_configuration")
                .with_title("Invalid Configuration")
                .with_detail(msg),
            GitProviderError::RateLimitExceeded => problem_new(StatusCode::TOO_MANY_REQUESTS)
                .with_type("https://docs.temps.sh/errors/rate_limit_exceeded")
                .with_title("Rate Limit Exceeded")
                .with_detail("You have exceeded the request rate limit for this provider."),
            GitProviderError::Other(msg) => problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_type("https://docs.temps.sh/errors/internal_error")
                .with_title("Internal Error")
                .with_detail(msg),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateProviderRequest {
    pub name: String,
    pub provider_type: String, // github, gitlab, bitbucket, gitea, generic
    pub auth_method: String,   // github_app, gitlab_app, oauth, pat, basic, ssh
    pub auth_config: serde_json::Value,
    pub base_url: Option<String>,
    pub api_url: Option<String>,
    pub webhook_secret: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateGitHubPATRequest {
    pub name: String,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateGitLabPATRequest {
    pub name: String,
    pub token: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateGitLabOAuthRequest {
    pub name: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProviderResponse {
    pub id: i32,
    pub name: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub auth_method: String,
    pub is_active: bool,
    pub is_default: bool,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectUsageInfoResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub connection_id: i32,
    pub connection_name: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProviderDeletionCheckResponse {
    pub can_delete: bool,
    pub projects_in_use: Vec<ProjectUsageInfoResponse>,
    pub message: String,
}

// Helper function to convert JSON preset cache to Vec<ProjectPresetResponse>
fn convert_preset_json(json: Option<serde_json::Value>) -> Option<Vec<ProjectPresetResponse>> {
    json.and_then(|value| serde_json::from_value::<Vec<ProjectPresetResponse>>(value).ok())
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ConnectionResponse {
    pub id: i32,
    pub provider_id: i32,
    pub user_id: Option<i32>,
    pub account_name: String,
    pub account_type: String,
    pub installation_id: Option<String>,
    pub is_active: bool,
    pub is_expired: bool,
    pub syncing: bool,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_synced_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RepositoryResponse {
    pub id: i32,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub default_branch: String,
    pub language: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub pushed_at: UtcDateTime,
    pub preset: Option<Vec<ProjectPresetResponse>>,
    /// HTTPS clone URL (e.g., https://github.com/owner/repo.git)
    pub clone_url: Option<String>,
    /// SSH clone URL (e.g., git@github.com:owner/repo.git)
    pub ssh_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RepositoryListResponse {
    pub repositories: Vec<RepositoryResponse>,
    pub total_count: usize,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RepositorySyncResponse {
    pub repositories: Vec<RepositoryResponse>,
    pub total_count: usize,
    #[schema(value_type = String, format = DateTime)]
    pub synced_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectPresetResponse {
    pub path: String,
    pub preset: String,
    pub preset_label: String,
    /// Default exposed port for this preset (e.g., 3000 for Next.js, 8000 for FastAPI)
    pub exposed_port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RepositoryPresetResponse {
    pub repository_id: i32,
    pub owner: String,
    pub name: String,
    pub presets: Vec<ProjectPresetResponse>,
    #[schema(value_type = String, format = DateTime)]
    pub calculated_at: UtcDateTime,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RepositoryListQuery {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
    pub sort: Option<String>,
    pub direction: Option<String>,
    pub search: Option<String>,
    pub owner: Option<String>,
    pub language: Option<String>,
    pub private: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncedRepositoryListQuery {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
    pub sort: Option<String>,
    pub direction: Option<String>,
    pub search: Option<String>,
    pub owner: Option<String>,
    pub language: Option<String>,
    pub private: Option<bool>,
    pub git_provider_connection_id: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ConnectionListQuery {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
    pub sort: Option<String>,
    pub direction: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ConnectionListResponse {
    pub connections: Vec<ConnectionResponse>,
    pub total_count: usize,
    pub page: u64,
    pub per_page: u64,
}

impl From<RepositoryListQuery> for RepositoryListParams {
    fn from(query: RepositoryListQuery) -> Self {
        Self {
            page: query.page,
            per_page: query.per_page.or(Some(30)), // Default to 30 per page
            sort: query.sort,
            direction: query.direction,
            organization: None, // Will be set from connection data
            search_term: query.search,
        }
    }
}

impl From<crate::services::git_provider_manager::ProjectUsageInfo> for ProjectUsageInfoResponse {
    fn from(info: crate::services::git_provider_manager::ProjectUsageInfo) -> Self {
        Self {
            id: info.id,
            name: info.name,
            slug: info.slug,
            connection_id: info.connection_id,
            connection_name: info.connection_name,
        }
    }
}

impl From<crate::services::git_provider_manager::ProviderDeletionCheck>
    for ProviderDeletionCheckResponse
{
    fn from(check: crate::services::git_provider_manager::ProviderDeletionCheck) -> Self {
        Self {
            can_delete: check.can_delete,
            projects_in_use: check.projects_in_use.into_iter().map(Into::into).collect(),
            message: check.message,
        }
    }
}

/// Create a new git provider configuration
#[utoipa::path(
    post,
    path = "/git-providers",
    request_body = CreateProviderRequest,
    responses(
        (status = 201, description = "Provider created successfully", body = ProviderResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_git_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersCreate);

    // Parse provider type
    let provider_type = GitProviderType::try_from(request.provider_type.as_str()).map_err(|e| {
        problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Provider Type")
            .with_detail(e.to_string())
    })?;

    // Parse auth method from config
    let auth_method =
        parse_auth_method(&request.auth_method, request.auth_config).map_err(|e| {
            problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Auth Configuration")
                .with_detail(e)
        })?;

    let provider = state
        .git_provider_manager
        .create_provider(
            request.name,
            provider_type,
            auth_method,
            request.base_url,
            request.api_url,
            request.webhook_secret,
            request.is_default,
        )
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ProviderResponse {
            id: provider.id,
            name: provider.name,
            provider_type: provider.provider_type,
            base_url: provider.base_url,
            auth_method: provider.auth_method,
            is_active: provider.is_active,
            is_default: provider.is_default,
            created_at: provider.created_at,
            updated_at: provider.updated_at,
        }),
    ))
}

/// List all git providers
#[utoipa::path(
    get,
    path = "/git-providers",
    responses(
        (status = 200, description = "List of providers", body = Vec<ProviderResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_git_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersRead);

    let providers = state.git_provider_manager.list_providers().await?;
    let response: Vec<ProviderResponse> = providers
        .into_iter()
        .map(|p| ProviderResponse {
            id: p.id,
            name: p.name,
            provider_type: p.provider_type,
            base_url: p.base_url,
            auth_method: p.auth_method,
            is_active: p.is_active,
            is_default: p.is_default,
            created_at: p.created_at,
            updated_at: p.updated_at,
        })
        .collect();
    Ok(Json(response))
}

/// Get a specific git provider
#[utoipa::path(
    get,
    path = "/git-providers/{provider_id}",
    params(
        ("provider_id" = i32, Path, description = "Provider ID")
    ),
    responses(
        (status = 200, description = "Provider details", body = ProviderResponse),
        (status = 404, description = "Provider not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_git_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersRead);

    let provider = state.git_provider_manager.get_provider(provider_id).await?;
    Ok(Json(ProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: provider.provider_type,
        base_url: provider.base_url,
        auth_method: provider.auth_method,
        is_active: provider.is_active,
        is_default: provider.is_default,
        created_at: provider.created_at,
        updated_at: provider.updated_at,
    }))
}

/// List user's git provider connections
#[utoipa::path(
    get,
    path = "/git-connections",
    params(
        ("page" = Option<u64>, Query, description = "Page number for pagination (default: 1)"),
        ("per_page" = Option<u64>, Query, description = "Number of items per page (default: 30, max: 100)"),
        ("sort" = Option<String>, Query, description = "Sort field (created_at, updated_at, account_name)"),
        ("direction" = Option<String>, Query, description = "Sort direction (asc, desc), default: desc")
    ),
    responses(
        (status = 200, description = "List of connections", body = ConnectionListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_connections(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ConnectionListQuery>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsRead);

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(30).min(100);
    let sort = query.sort.as_deref().unwrap_or("created_at");
    let direction = query.direction.as_deref().unwrap_or("desc");

    let (connections, total_count) = state
        .git_provider_manager
        .get_user_connections_paginated(page, per_page, sort, direction)
        .await?;

    let response_connections: Vec<ConnectionResponse> = connections
        .into_iter()
        .map(|conn| ConnectionResponse {
            id: conn.id,
            provider_id: conn.provider_id,
            user_id: conn.user_id,
            account_name: conn.account_name,
            account_type: conn.account_type,
            installation_id: conn.installation_id,
            is_active: conn.is_active,
            is_expired: conn.is_expired,
            syncing: conn.syncing,
            last_synced_at: conn.last_synced_at,
            created_at: conn.created_at,
            updated_at: conn.updated_at,
        })
        .collect();

    Ok(Json(ConnectionListResponse {
        connections: response_connections,
        total_count,
        page,
        per_page,
    }))
}

/// Sync repositories from a connection
///
/// Synchronizes repository data from the git provider to the local database.
/// This updates the local cache of repositories for faster access.
#[utoipa::path(
    post,
    path = "/git-connections/{connection_id}/sync",
    params(
        ("connection_id" = i32, Path, description = "Connection ID")
    ),
    responses(
        (status = 200, description = "Repositories synced successfully", body = RepositorySyncResponse),
        (status = 404, description = "Connection not found"),
        (status = 409, description = "Sync already in progress"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn sync_repositories(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitRepositoriesSync);

    // Use the standard sync for all providers, including GitHub Apps
    // The git_provider_manager now handles GitHub App installation token generation internally
    let repositories = state
        .git_provider_manager
        .sync_repositories(connection_id)
        .await?;
    let items: Vec<RepositoryResponse> = repositories
        .iter()
        .map(|r| RepositoryResponse {
            id: r.id,
            owner: r.owner.clone(),
            name: r.name.clone(),
            full_name: r.full_name.clone(),
            description: r.description.clone(),
            private: r.private,
            default_branch: r.default_branch.clone(),
            language: r.language.clone(),
            created_at: r.created_at,
            updated_at: r.updated_at,
            pushed_at: r.pushed_at,
            preset: convert_preset_json(r.preset.clone()),
            clone_url: r.clone_url.clone(),
            ssh_url: r.ssh_url.clone(),
        })
        .collect();

    let total_count = items.len();
    Ok(Json(RepositorySyncResponse {
        repositories: items,
        total_count,
        synced_at: chrono::Utc::now(),
    }))
}

/// List repositories for a specific connection
///
/// Fetches repositories from the connected git provider with support for pagination, search, and filtering.
/// This endpoint calls the provider's API directly to get the most up-to-date repository list.
#[utoipa::path(
    get,
    path = "/git-connections/{connection_id}/repositories",
    params(
        ("connection_id" = i32, Path, description = "Connection ID"),
        ("page" = Option<u64>, Query, description = "Page number for pagination"),
        ("per_page" = Option<u64>, Query, description = "Number of items per page (max 100)"),
        ("sort" = Option<String>, Query, description = "Sort field (name, created_at, updated_at, stars, etc.)"),
        ("direction" = Option<String>, Query, description = "Sort direction (asc, desc)"),
        ("search" = Option<String>, Query, description = "Search term to filter repositories"),
        ("owner" = Option<String>, Query, description = "Filter by repository owner"),
        ("language" = Option<String>, Query, description = "Filter by programming language"),
        ("private" = Option<bool>, Query, description = "Filter by private status (true/false)")
    ),
    responses(
        (status = 200, description = "List of repositories", body = RepositoryListResponse),
        (status = 404, description = "Connection not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_repositories_by_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<i32>,
    Query(query): Query<RepositoryListQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitRepositoriesRead);

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(30).min(100);

    // Build service-layer filter from query parameters
    let sort = match (
        query.sort.as_deref().unwrap_or("updated_at"),
        query.direction.as_deref().unwrap_or("desc"),
    ) {
        ("name", "asc") => Some("name".to_string()),
        ("name", "desc") => Some("name_desc".to_string()),
        ("created_at", "asc") => Some("created".to_string()),
        ("created_at", "desc") => Some("created_desc".to_string()),
        ("updated_at", "asc") => Some("updated".to_string()),
        ("updated_at", "desc") => Some("updated_desc".to_string()),
        ("pushed_at", "asc") => Some("pushed".to_string()),
        ("pushed_at", "desc") => Some("pushed_desc".to_string()),
        ("stars", "asc") => Some("stars".to_string()),
        ("stars", "desc") => Some("stars_desc".to_string()),
        ("watchers", "asc") => Some("watchers".to_string()),
        ("watchers", "desc") => Some("watchers_desc".to_string()),
        ("size", "asc") => Some("size".to_string()),
        ("size", "desc") => Some("size_desc".to_string()),
        ("issues", "asc") => Some("issues".to_string()),
        ("issues", "desc") => Some("issues_desc".to_string()),
        _ => Some("updated_desc".to_string()), // Default
    };
    let filter = RepositoryFilter {
        git_provider_connection_id: Some(connection_id),
        search: query.search.clone(),
        owner: query.owner.clone(),
        language: query.language.clone(),
        private: query.private,
        sort,
        limit: Some(per_page),
        offset: Some((page - 1) * per_page),
    };

    // Use repository service for fast database query instead of API calls
    let repository_models = state.repository_service.list_repositories(filter).await?;

    // For total count, we need to make a separate call without pagination
    let count_filter = RepositoryFilter {
        git_provider_connection_id: Some(connection_id),
        search: query.search.clone(),
        owner: query.owner.clone(),
        language: query.language.clone(),
        private: query.private,
        sort: None,
        limit: None,
        offset: None,
    };
    let all_repositories = state
        .repository_service
        .list_repositories(count_filter)
        .await?;
    let total_count = all_repositories.len();

    // Convert service models to HTTP response format
    let repositories: Vec<RepositoryResponse> = repository_models
        .into_iter()
        .map(|r| RepositoryResponse {
            id: r.id,
            owner: r.owner,
            name: r.name,
            full_name: r.full_name,
            description: r.description,
            private: r.private,
            default_branch: r.default_branch,
            language: r.language,
            created_at: r.created_at,
            updated_at: r.updated_at,
            pushed_at: r.pushed_at,
            preset: convert_preset_json(r.preset.clone()),
            clone_url: r.clone_url,
            ssh_url: r.ssh_url,
        })
        .collect();

    Ok(Json(RepositoryListResponse {
        repositories,
        total_count,
    }))
}

/// List all repositories for a specific provider
#[utoipa::path(
    get,
    path = "/git-providers/{provider_id}/repositories",
    params(
        ("provider_id" = i32, Path, description = "Provider ID")
    ),
    responses(
        (status = 200, description = "List of repositories", body = RepositoryListResponse),
        (status = 404, description = "Provider not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_repositories_by_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitRepositoriesRead);

    // First verify the provider exists
    state.git_provider_manager.get_provider(provider_id).await?;

    // Use the repository service to get repositories
    let repositories = state
        .repository_service
        .list_repositories_by_provider(provider_id)
        .await?;

    let response: Vec<RepositoryResponse> = repositories
        .into_iter()
        .map(|r| RepositoryResponse {
            id: r.id,
            owner: r.owner,
            name: r.name,
            full_name: r.full_name,
            description: r.description,
            private: r.private,
            default_branch: r.default_branch,
            language: r.language,
            created_at: r.created_at,
            updated_at: r.updated_at,
            pushed_at: r.pushed_at,
            preset: convert_preset_json(r.preset.clone()),
            clone_url: r.clone_url,
            ssh_url: r.ssh_url,
        })
        .collect();
    Ok(Json(response))
}

/// List synced repositories with advanced filtering
///
/// Lists repositories that have been synced to the database with comprehensive filtering options.
/// This provides fast access to repository metadata with filtering by connection, search, and other criteria.
#[utoipa::path(
    get,
    path = "/repositories",
    params(
        ("page" = Option<u64>, Query, description = "Page number for pagination"),
        ("per_page" = Option<u64>, Query, description = "Number of items per page (max 100)"),
        ("sort" = Option<String>, Query, description = "Sort field (name, created_at, updated_at, stars, watchers, size, issues)"),
        ("direction" = Option<String>, Query, description = "Sort direction (asc, desc)"),
        ("search" = Option<String>, Query, description = "Search term to filter repositories"),
        ("owner" = Option<String>, Query, description = "Filter by repository owner"),
        ("language" = Option<String>, Query, description = "Filter by programming language"),
        ("private" = Option<bool>, Query, description = "Filter by private status (true/false)"),
        ("git_provider_connection_id" = Option<i32>, Query, description = "Filter by git provider connection ID")
    ),
    responses(
        (status = 200, description = "List of synced repositories", body = RepositoryListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_synced_repositories(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<SyncedRepositoryListQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitRepositoriesRead);

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(30).min(100);

    // Build service-layer filter from query parameters
    let sort = match (
        query.sort.as_deref().unwrap_or("updated_at"),
        query.direction.as_deref().unwrap_or("desc"),
    ) {
        ("name", "asc") => Some("name".to_string()),
        ("name", "desc") => Some("name_desc".to_string()),
        ("created_at", "asc") => Some("created".to_string()),
        ("created_at", "desc") => Some("created_desc".to_string()),
        ("updated_at", "asc") => Some("updated".to_string()),
        ("updated_at", "desc") => Some("updated_desc".to_string()),
        ("stars", "asc") => Some("stars".to_string()),
        ("stars", "desc") => Some("stars_desc".to_string()),
        ("watchers", "asc") => Some("watchers".to_string()),
        ("watchers", "desc") => Some("watchers_desc".to_string()),
        ("size", "asc") => Some("size".to_string()),
        ("size", "desc") => Some("size_desc".to_string()),
        ("issues", "asc") => Some("issues".to_string()),
        ("issues", "desc") => Some("issues_desc".to_string()),
        _ => Some("updated_desc".to_string()), // Default
    };

    let filter = RepositoryFilter {
        git_provider_connection_id: query.git_provider_connection_id,
        search: query.search.clone(),
        owner: query.owner.clone(),
        language: query.language.clone(),
        private: query.private,
        sort,
        limit: Some(per_page),
        offset: Some((page - 1) * per_page),
    };

    // Use repository service instead of direct database access
    let repository_models = state.repository_service.list_repositories(filter).await?;

    // For total count, we need to make a separate call without pagination
    let count_filter = RepositoryFilter {
        git_provider_connection_id: query.git_provider_connection_id,
        search: query.search.clone(),
        owner: query.owner.clone(),
        language: query.language.clone(),
        private: query.private,
        sort: None,
        limit: None,
        offset: None,
    };
    let all_repositories = state
        .repository_service
        .list_repositories(count_filter)
        .await?;
    let total_count = all_repositories.len();

    // Convert service models to HTTP response format
    let repositories: Vec<RepositoryResponse> = repository_models
        .into_iter()
        .map(|r| RepositoryResponse {
            id: r.id,
            owner: r.owner,
            name: r.name,
            full_name: r.full_name,
            description: r.description,
            private: r.private,
            default_branch: r.default_branch,
            language: r.language,
            created_at: r.created_at,
            updated_at: r.updated_at,
            pushed_at: r.pushed_at,
            preset: convert_preset_json(r.preset.clone()),
            clone_url: r.clone_url,
            ssh_url: r.ssh_url,
        })
        .collect();

    Ok(Json(RepositoryListResponse {
        repositories,
        total_count,
    }))
}

/// Get repository preset by owner and name
#[utoipa::path(
    get,
    path = "/repositories/{owner}/{name}/preset",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("branch" = Option<String>, Query, description = "Git branch to check (defaults to repository's default branch)")
    ),
    responses(
        (status = 200, description = "Repository preset calculated successfully", body = RepositoryPresetResponse),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_repository_preset_by_name(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<PresetLiveQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitRepositoriesRead);

    // First, find the repository from any available connection
    let repository = state
        .repository_service
        .find_by_owner_and_name(&owner, &name)
        .await
        .map_err(|e| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to find repository")
                .with_detail(e.to_string())
        })?
        .ok_or_else(|| {
            problem_new(StatusCode::NOT_FOUND)
                .with_title("Repository not found")
                .with_detail(format!("Repository {}/{} not found", owner, name))
        })?;

    // Calculate preset for this repository
    let preset_result = state
        .git_provider_manager
        .calculate_repository_preset_live(repository.id, query.branch)
        .await?;

    Ok((
        StatusCode::OK,
        Json(RepositoryPresetResponse {
            repository_id: preset_result.repository_id,
            owner: preset_result.owner,
            name: preset_result.name,
            presets: preset_result
                .presets
                .into_iter()
                .map(|p| ProjectPresetResponse {
                    path: p.path,
                    preset: p.preset,
                    preset_label: p.preset_label,
                    exposed_port: p.exposed_port,
                })
                .collect(),
            calculated_at: preset_result.calculated_at,
        }),
    ))
}

/// Get repository by owner and name from any connection
#[utoipa::path(
    get,
    path = "/repositories/{owner}/{name}",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("connection_id" = Option<i32>, Query, description = "Optional specific connection ID to search")
    ),
    responses(
        (status = 200, description = "Repository found", body = RepositoryResponse),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_repository_by_name(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitRepositoriesRead);

    let connection_id = query
        .get("connection_id")
        .and_then(|id| id.parse::<i32>().ok());

    // Find the repository, optionally from a specific connection
    let repository = if let Some(conn_id) = connection_id {
        state
            .repository_service
            .find_by_owner_and_name_in_connection(&owner, &name, conn_id)
            .await
            .map_err(|e| {
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to find repository")
                    .with_detail(e.to_string())
            })?
    } else {
        state
            .repository_service
            .find_by_owner_and_name(&owner, &name)
            .await
            .map_err(|e| {
                problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed to find repository")
                    .with_detail(e.to_string())
            })?
    };

    let repository = repository.ok_or_else(|| {
        problem_new(StatusCode::NOT_FOUND)
            .with_title("Repository not found")
            .with_detail(format!("Repository {}/{} not found", owner, name))
    })?;

    Ok((
        StatusCode::OK,
        Json(RepositoryResponse {
            id: repository.id,
            owner: repository.owner,
            name: repository.name,
            full_name: repository.full_name,
            description: repository.description,
            private: repository.private,
            default_branch: repository.default_branch,
            language: repository.language,
            created_at: repository.created_at,
            updated_at: repository.updated_at,
            pushed_at: repository.pushed_at,
            preset: convert_preset_json(repository.preset),
            clone_url: repository.clone_url,
            ssh_url: repository.ssh_url,
        }),
    ))
}

/// Get all repositories with same owner/name from all git providers
#[utoipa::path(
    get,
    path = "/repositories/{owner}/{name}/all",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name")
    ),
    responses(
        (status = 200, description = "Repositories found from all providers", body = Vec<RepositoryResponse>),
        (status = 404, description = "No repositories found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_all_repositories_by_name(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((owner, name)): Path<(String, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitRepositoriesRead);

    // Find all repositories with this owner/name across all git provider connections
    let repositories = state
        .repository_service
        .find_all_by_owner_and_name(&owner, &name)
        .await
        .map_err(|e| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to find repositories")
                .with_detail(e.to_string())
        })?;

    if repositories.is_empty() {
        return Err(problem_new(StatusCode::NOT_FOUND)
            .with_title("No repositories found")
            .with_detail(format!(
                "No repositories named {}/{} found in any git provider",
                owner, name
            )));
    }

    let response: Vec<RepositoryResponse> = repositories
        .into_iter()
        .map(|repository| RepositoryResponse {
            id: repository.id,
            owner: repository.owner,
            name: repository.name,
            full_name: repository.full_name,
            description: repository.description,
            private: repository.private,
            default_branch: repository.default_branch,
            language: repository.language,
            created_at: repository.created_at,
            updated_at: repository.updated_at,
            pushed_at: repository.pushed_at,
            preset: convert_preset_json(repository.preset),
            clone_url: repository.clone_url,
            ssh_url: repository.ssh_url,
        })
        .collect();

    Ok((StatusCode::OK, Json(response)))
}

/// Configure routes for git providers
pub fn configure_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{delete, get, post};

    axum::Router::new()
        // Provider configuration management
        .route(
            "/git-providers",
            post(create_git_provider).get(list_git_providers),
        )
        .route(
            "/git-providers/{provider_id}",
            get(get_git_provider).delete(delete_provider),
        )
        .route(
            "/git-providers/{provider_id}/deletion-check",
            get(check_provider_deletion_safety),
        )
        .route(
            "/git-providers/{provider_id}/safe-delete",
            delete(delete_provider_safely),
        )
        .route(
            "/git-providers/{provider_id}/deactivate",
            post(deactivate_provider),
        )
        .route(
            "/git-providers/{provider_id}/activate",
            post(activate_provider),
        )
        // Quick provider creation shortcuts
        .route(
            "/git-providers/github/pat",
            post(create_github_pat_provider),
        )
        .route(
            "/git-providers/gitlab/pat",
            post(create_gitlab_pat_provider),
        )
        .route(
            "/git-providers/gitlab/oauth",
            post(create_gitlab_oauth_provider),
        )
        // OAuth flow (provider-specific as it configures the provider)
        .route(
            "/git-providers/{provider_id}/oauth/authorize",
            get(start_git_provider_oauth),
        )
        .route(
            "/git-providers/{provider_id}/callback",
            get(handle_git_provider_oauth_callback),
        )
        // Connection management - available at both locations
        // Under /connections for direct access to user's connections
        .route("/git-connections", get(list_connections))
        .route(
            "/git-connections/{connection_id}",
            delete(delete_connection),
        )
        .route(
            "/git-connections/{connection_id}/deactivate",
            post(deactivate_connection),
        )
        .route(
            "/git-connections/{connection_id}/activate",
            post(activate_connection),
        )
        .route(
            "/git-connections/{connection_id}/sync",
            post(sync_repositories),
        )
        .route(
            "/git-connections/{connection_id}/repositories",
            get(list_repositories_by_connection),
        )
        .route(
            "/git-connections/{connection_id}/update-token",
            post(update_connection_token),
        )
        .route(
            "/git-connections/{connection_id}/validate",
            get(validate_connection),
        )
        // Repository listing with advanced filtering
        .route("/repositories", get(list_synced_repositories))
        // Repository preset calculation
        .route(
            "/repositories/{repository_id}/preset/live",
            get(get_repository_preset_live),
        )
        // Repository preset by owner/name
        .route(
            "/repositories/{owner}/{name}/preset",
            get(get_repository_preset_by_name),
        )
        // Get repository from any connection by owner/name
        .route("/repositories/{owner}/{name}", get(get_repository_by_name))
        // Get all repositories with same owner/name from all providers
        .route(
            "/repositories/{owner}/{name}/all",
            get(get_all_repositories_by_name),
        )
        // Also under /git-providers/connections for creating connections (shows hierarchy)
        .route(
            "/git-providers/{provider_id}/connections",
            get(get_provider_connections),
        )
        .route(
            "/repositories/{owner}/{repo}/branches",
            get(get_repository_branches),
        )
        .route(
            "/repositories/{owner}/{repo}/tags",
            get(get_repository_tags),
        )
        // New endpoints using repository ID (singular)
        .route(
            "/repository/{repository_id}/branches",
            get(get_branches_by_repository_id),
        )
        .route(
            "/repository/{repository_id}/tags",
            get(get_tags_by_repository_id),
        )
        .route(
            "/repository/{repository_id}/commits/{commit_sha}",
            get(check_commit_exists),
        )
}

// Helper function to parse auth method
/// Start OAuth flow for a git provider
#[utoipa::path(
    get,
    path = "/git-providers/{provider_id}/oauth/authorize",
    params(
        ("provider_id" = i32, Path, description = "Git provider ID")
    ),
    responses(
        (status = 302, description = "Redirect to OAuth provider"),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn start_git_provider_oauth(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsCreate);

    // Extract the host from request headers for dynamic callback URL
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(|host| {
            let scheme = headers
                .get("x-forwarded-proto")
                .and_then(|p| p.to_str().ok())
                .unwrap_or("https");
            format!("{}://{}/api", scheme, host)
        });

    let (auth_url, _state) = state
        .git_provider_manager
        .start_oauth_flow(provider_id, host)
        .await?;
    Ok(axum::response::Redirect::to(&auth_url))
}

/// Handle OAuth callback for a git provider
#[utoipa::path(
    get,
    path = "/git-providers/{provider_id}/callback",
    params(
        ("provider_id" = i32, Path, description = "Git provider ID"),
        ("code" = String, Query, description = "OAuth authorization code"),
        ("state" = String, Query, description = "CSRF state token")
    ),
    responses(
        (status = 302, description = "Redirect to success page"),
        (status = 400, description = "Bad request"),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers"
)]
pub async fn handle_git_provider_oauth_callback(
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsCreate);

    let code = params.get("code").cloned().unwrap_or_default();
    let oauth_state = params.get("state").cloned().unwrap_or_default();

    if code.is_empty() {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Missing Authorization Code")
            .with_detail("OAuth authorization code is required"));
    }

    // Extract the host from request headers for consistent callback URL
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(|host| {
            let scheme = headers
                .get("x-forwarded-proto")
                .and_then(|p| p.to_str().ok())
                .unwrap_or("https");
            format!("{}://{}/api", scheme, host)
        });

    let connection = state
        .git_provider_manager
        .handle_oauth_callback(provider_id, code, oauth_state, auth.user.id, host)
        .await?;

    // Redirect to success page or dashboard
    let redirect_url = format!(
        "/git-providers/{}/connections/{}",
        provider_id, connection.id
    );
    Ok(axum::response::Redirect::to(&redirect_url))
}

fn parse_auth_method(method_type: &str, config: serde_json::Value) -> Result<AuthMethod, String> {
    match method_type {
        "github_app" => Ok(AuthMethod::GitHubApp {
            app_id: config["app_id"].as_i64().ok_or("app_id required")? as i32,
            client_id: config["client_id"]
                .as_str()
                .ok_or("client_id required")?
                .to_string(),
            client_secret: config["client_secret"]
                .as_str()
                .ok_or("client_secret required")?
                .to_string(),
            private_key: config["private_key"]
                .as_str()
                .ok_or("private_key required")?
                .to_string(),
            webhook_secret: config["webhook_secret"]
                .as_str()
                .ok_or("webhook_secret required")?
                .to_string(),
        }),
        "gitlab_app" => Ok(AuthMethod::GitLabApp {
            app_id: config["app_id"]
                .as_str()
                .ok_or("app_id required")?
                .to_string(),
            app_secret: config["app_secret"]
                .as_str()
                .ok_or("app_secret required")?
                .to_string(),
            redirect_uri: config["redirect_uri"]
                .as_str()
                .ok_or("redirect_uri required")?
                .to_string(),
        }),
        "oauth" => Ok(AuthMethod::OAuth {
            client_id: config["client_id"]
                .as_str()
                .ok_or("client_id required")?
                .to_string(),
            client_secret: config["client_secret"]
                .as_str()
                .ok_or("client_secret required")?
                .to_string(),
            redirect_uri: config["redirect_uri"]
                .as_str()
                .ok_or("redirect_uri required")?
                .to_string(),
        }),
        "pat" => Ok(AuthMethod::PersonalAccessToken {
            token: config["token"]
                .as_str()
                .ok_or("token required")?
                .to_string(),
        }),
        "basic" => Ok(AuthMethod::BasicAuth {
            username: config["username"]
                .as_str()
                .ok_or("username required")?
                .to_string(),
            password: config["password"]
                .as_str()
                .ok_or("password required")?
                .to_string(),
        }),
        "ssh" => Ok(AuthMethod::SSHKey {
            private_key: config["private_key"]
                .as_str()
                .ok_or("private_key required")?
                .to_string(),
            public_key: config["public_key"]
                .as_str()
                .ok_or("public_key required")?
                .to_string(),
        }),
        _ => Err(format!("Unknown auth method: {}", method_type)),
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(
    nest (
        (path = "/", api = crate::handlers::repositories::RepositoriesApiDoc)
    ),
    paths(
        create_git_provider,
        list_git_providers,
        get_git_provider,
        start_git_provider_oauth,
        handle_git_provider_oauth_callback,
        list_connections,
        get_provider_connections,
        sync_repositories,
        list_repositories_by_connection,
        list_synced_repositories,
        get_repository_preset_live,
        get_repository_preset_by_name,
        get_repository_by_name,
        get_all_repositories_by_name,
        create_github_pat_provider,
        create_gitlab_pat_provider,
        create_gitlab_oauth_provider,
        delete_provider,
        check_provider_deletion_safety,
        delete_provider_safely,
        deactivate_provider,
        activate_provider,
        deactivate_connection,
        activate_connection,
        delete_connection,
        update_connection_token,
        validate_connection,
    ),
    components(
        schemas(
            CreateProviderRequest,
            ProviderResponse,
            ConnectionResponse,
            ConnectionListQuery,
            ConnectionListResponse,
            RepositoryResponse,
            RepositoryPresetResponse,
            ProjectPresetResponse,
            RepositoryListQuery,
            SyncedRepositoryListQuery,
            RepositoryListResponse,
            CreateGitHubPATRequest,
            CreateGitLabPATRequest,
            CreateGitLabOAuthRequest,
            ProjectUsageInfoResponse,
            ProviderDeletionCheckResponse,
            UpdateTokenRequest,
            UpdateTokenResponse,
            ValidationResponse,
        )
    ),
    tags(
        (name = "Git Providers", description = "Git provider management endpoints"),
    )
)]
pub struct GitProvidersApiDoc;

/// Create a GitHub Personal Access Token provider
#[utoipa::path(
    post,
    path = "/git-providers/github/pat",
    request_body = CreateGitHubPATRequest,
    responses(
        (status = 201, description = "GitHub PAT provider created successfully", body = ProviderResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_github_pat_provider(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateGitHubPATRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersCreate);

    let user_id = auth.user.id;

    let provider = state
        .git_provider_manager
        .create_github_pat_provider(request.name.clone(), request.token, user_id)
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ProviderResponse {
            id: provider.id,
            name: provider.name,
            provider_type: provider.provider_type,
            base_url: provider.base_url,
            auth_method: provider.auth_method,
            is_active: provider.is_active,
            is_default: provider.is_default,
            created_at: provider.created_at,
            updated_at: provider.updated_at,
        }),
    ))
}

/// Create a GitLab PAT provider
#[utoipa::path(
    post,
    path = "/git-providers/gitlab/pat",
    request_body = CreateGitLabPATRequest,
    responses(
        (status = 201, description = "GitLab PAT provider created successfully", body = ProviderResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_gitlab_pat_provider(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateGitLabPATRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersCreate);

    let user_id = auth.user.id;

    let provider = state
        .git_provider_manager
        .create_gitlab_pat_provider(
            request.name.clone(),
            request.token,
            user_id,
            request.base_url,
        )
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ProviderResponse {
            id: provider.id,
            name: provider.name,
            provider_type: provider.provider_type,
            base_url: provider.base_url,
            auth_method: provider.auth_method,
            is_active: provider.is_active,
            is_default: provider.is_default,
            created_at: provider.created_at,
            updated_at: provider.updated_at,
        }),
    ))
}

/// Create a GitLab OAuth provider
#[utoipa::path(
    post,
    path = "/git-providers/gitlab/oauth",
    request_body = CreateGitLabOAuthRequest,
    responses(
        (status = 201, description = "GitLab OAuth provider created successfully", body = ProviderResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_gitlab_oauth_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateGitLabOAuthRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersCreate);

    let provider = state
        .git_provider_manager
        .create_gitlab_oauth_provider(
            request.name.clone(),
            request.client_id,
            request.client_secret,
            request.redirect_uri,
            request.base_url,
        )
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(ProviderResponse {
            id: provider.id,
            name: provider.name,
            provider_type: provider.provider_type,
            base_url: provider.base_url,
            auth_method: provider.auth_method,
            is_active: provider.is_active,
            is_default: provider.is_default,
            created_at: provider.created_at,
            updated_at: provider.updated_at,
        }),
    ))
}

#[derive(Debug, serde::Deserialize)]
pub struct PresetLiveQuery {
    pub branch: Option<String>,
}

#[utoipa::path(
    get,
    path = "/repositories/{repository_id}/preset/live",
    params(
        ("repository_id" = i32, Path, description = "Repository ID"),
        ("branch" = Option<String>, Query, description = "Git branch to check (defaults to repository's default branch)"),
    ),
    responses(
        (status = 200, description = "Repository presets calculated successfully - includes root preset and projects in subdirectories", body = RepositoryPresetResponse),
        (status = 404, description = "Repository not found"),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_repository_preset_live(
    State(state): State<Arc<AppState>>,
    Path(repository_id): Path<i32>,
    Query(query): Query<PresetLiveQuery>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitRepositoriesRead);

    // Use only the service layer - no direct database access
    let preset_result = state
        .git_provider_manager
        .calculate_repository_preset_live(repository_id, query.branch)
        .await?;

    Ok((
        StatusCode::OK,
        Json(RepositoryPresetResponse {
            repository_id: preset_result.repository_id,
            owner: preset_result.owner,
            name: preset_result.name,
            presets: preset_result
                .presets
                .into_iter()
                .map(|p| ProjectPresetResponse {
                    path: p.path,
                    preset: p.preset,
                    preset_label: p.preset_label,
                    exposed_port: p.exposed_port,
                })
                .collect(),
            calculated_at: preset_result.calculated_at,
        }),
    ))
}
/// Get connections for a specific git provider
#[utoipa::path(
    get,
    path = "/git-providers/{provider_id}/connections",
    params(
        ("provider_id" = i32, Path, description = "Provider ID to get connections for")
    ),
    responses(
        (status = 200, description = "List of connections for the provider", body = Vec<ConnectionResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_provider_connections(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsRead);

    let connections = state
        .git_provider_manager
        .get_provider_connections(provider_id)
        .await?;

    let response: Vec<ConnectionResponse> = connections
        .into_iter()
        .map(|conn| ConnectionResponse {
            id: conn.id,
            provider_id: conn.provider_id,
            user_id: conn.user_id,
            account_name: conn.account_name,
            account_type: conn.account_type,
            installation_id: conn.installation_id,
            is_active: conn.is_active,
            is_expired: conn.is_expired,
            syncing: conn.syncing,
            last_synced_at: conn.last_synced_at,
            created_at: conn.created_at,
            updated_at: conn.updated_at,
        })
        .collect();

    Ok((StatusCode::OK, Json(response)))
}

/// Deactivate a git provider
#[utoipa::path(
    post,
    path = "/git-providers/{provider_id}/deactivate",
    params(
        ("provider_id" = i32, Path, description = "Provider ID")
    ),
    responses(
        (status = 200, description = "Provider deactivated successfully"),
        (status = 404, description = "Provider not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn deactivate_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersWrite);

    state
        .git_provider_manager
        .deactivate_provider(provider_id)
        .await?;
    Ok(StatusCode::OK)
}

/// Activate a git provider
#[utoipa::path(
    post,
    path = "/git-providers/{provider_id}/activate",
    params(
        ("provider_id" = i32, Path, description = "Provider ID")
    ),
    responses(
        (status = 200, description = "Provider activated successfully"),
        (status = 404, description = "Provider not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn activate_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersWrite);

    state
        .git_provider_manager
        .activate_provider(provider_id)
        .await?;
    Ok(StatusCode::OK)
}

/// Permanently delete a git provider
#[utoipa::path(
    delete,
    path = "/git-providers/{provider_id}",
    params(
        ("provider_id" = i32, Path, description = "Provider ID")
    ),
    responses(
        (status = 204, description = "Provider deleted successfully"),
        (status = 400, description = "Provider has connections and cannot be deleted"),
        (status = 404, description = "Provider not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersDelete);

    state
        .git_provider_manager
        .delete_provider(provider_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Check if a git provider can be safely deleted
#[utoipa::path(
    get,
    path = "/git-providers/{provider_id}/deletion-check",
    params(
        ("provider_id" = i32, Path, description = "Git provider ID")
    ),
    responses(
        (status = 200, description = "Deletion check result", body = ProviderDeletionCheckResponse),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn check_provider_deletion_safety(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersDelete);
    let check_result = state
        .git_provider_manager
        .check_provider_deletion_safety(provider_id)
        .await?;
    Ok(Json(ProviderDeletionCheckResponse::from(check_result)))
}

/// Safely delete a git provider (only if no projects are using it)
#[utoipa::path(
    delete,
    path = "/git-providers/{provider_id}/safe-delete",
    params(
        ("provider_id" = i32, Path, description = "Git provider ID")
    ),
    responses(
        (status = 204, description = "Provider successfully deleted"),
        (status = 400, description = "Cannot delete provider because it's in use"),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_provider_safely(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(provider_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitProvidersDelete);

    state
        .git_provider_manager
        .delete_provider_safely(provider_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deactivate a git provider connection
#[utoipa::path(
    post,
    path = "/git-connections/{connection_id}/deactivate",
    params(
        ("connection_id" = i32, Path, description = "Connection ID")
    ),
    responses(
        (status = 200, description = "Connection deactivated successfully"),
        (status = 400, description = "Connection is in use by projects and cannot be deactivated"),
        (status = 404, description = "Connection not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn deactivate_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsWrite);

    state
        .git_provider_manager
        .deactivate_connection(connection_id)
        .await?;
    Ok(StatusCode::OK)
}

/// Activate a git provider connection
#[utoipa::path(
    post,
    path = "/git-connections/{connection_id}/activate",
    params(
        ("connection_id" = i32, Path, description = "Connection ID")
    ),
    responses(
        (status = 200, description = "Connection activated successfully"),
        (status = 400, description = "Provider is deactivated and connection cannot be activated"),
        (status = 404, description = "Connection not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn activate_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsWrite);

    state
        .git_provider_manager
        .activate_connection(connection_id)
        .await?;
    Ok(StatusCode::OK)
}

/// Permanently delete a git provider connection
#[utoipa::path(
    delete,
    path = "/git-connections/{connection_id}",
    params(
        ("connection_id" = i32, Path, description = "Connection ID")
    ),
    responses(
        (status = 204, description = "Connection deleted successfully"),
        (status = 400, description = "Connection is in use by projects and cannot be deleted"),
        (status = 404, description = "Connection not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(connection_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsDelete);

    state
        .git_provider_manager
        .delete_connection(connection_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// From implementation is already in base.rs

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateTokenRequest {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateTokenResponse {
    pub connection_id: i32,
    pub message: String,
    pub is_active: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ValidationResponse {
    pub connection_id: i32,
    pub is_valid: bool,
    pub message: String,
}

/// Update access token for a connection (when tokens expire or are rotated)
#[utoipa::path(
    post,
    path = "/git-connections/{connection_id}/update-token",
    params(
        ("connection_id" = i32, Path, description = "Connection ID")
    ),
    request_body = UpdateTokenRequest,
    responses(
        (status = 200, description = "Token updated successfully", body = UpdateTokenResponse),
        (status = 404, description = "Connection not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Provider Connections",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_connection_token(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<GitAppState>>,
    Path(connection_id): Path<i32>,
    Json(request): Json<UpdateTokenRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsWrite);

    state
        .git_provider_manager
        .update_connection_token(connection_id, request.access_token, request.refresh_token)
        .await?;
    Ok(Json(UpdateTokenResponse {
        connection_id,
        message: "Token updated successfully".to_string(),
        is_active: true,
    }))
}

/// Validate a connection by testing the access token
#[utoipa::path(
    get,
    path = "/git-connections/{connection_id}/validate",
    params(
        ("connection_id" = i32, Path, description = "Connection ID")
    ),
    responses(
        (status = 200, description = "Connection validation result", body = ValidationResponse),
        (status = 404, description = "Connection not found"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Provider Connections",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn validate_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<GitAppState>>,
    Path(connection_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::GitConnectionsRead);

    let is_valid = state
        .git_provider_manager
        .validate_connection(connection_id)
        .await?;
    Ok(Json(ValidationResponse {
        connection_id,
        is_valid,
        message: if is_valid {
            "Connection is valid".to_string()
        } else {
            "Connection is invalid or token expired".to_string()
        },
    }))
}
