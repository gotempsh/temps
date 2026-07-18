use super::types::GitAppState as AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_check, AuthContext, AuthSource, Permission, RequireAuth};
use temps_core::{error_builder::ErrorBuilder, problemdetails::Problem};
use tracing::warn;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::services::{
    cache::CommitCacheKey,
    git_provider::{Commit, GitProviderError},
};

#[derive(Debug, Deserialize, IntoParams)]
pub struct ConnectionQueryParams {
    /// Git provider connection ID (required when multiple connections have the same repo)
    pub connection_id: i32,
    /// Force fetch fresh data, bypassing cache (default: false)
    #[serde(default)]
    pub fresh: bool,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct FreshQueryParams {
    /// Force fetch fresh data, bypassing cache (default: false)
    #[serde(default)]
    pub fresh: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BranchInfo {
    pub name: String,
    pub commit_sha: String,
    pub protected: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BranchListResponse {
    pub branches: Vec<BranchInfo>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TagInfo {
    pub name: String,
    pub commit_sha: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TagListResponse {
    pub tags: Vec<TagInfo>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CommitExistsResponse {
    pub exists: bool,
    pub commit_sha: Option<String>,
    /// Commit metadata when the requested SHA exists.
    pub commit: Option<CommitInfo>,
}

impl CommitExistsResponse {
    fn missing() -> Self {
        Self {
            exists: false,
            commit_sha: None,
            commit: None,
        }
    }

    fn found(commit: Commit) -> Self {
        let commit_sha = commit.sha.clone();
        Self {
            exists: true,
            commit_sha: Some(commit_sha),
            commit: Some(commit.into()),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum CommitShaValidationError {
    #[error("commit SHA must contain between 7 and 40 hexadecimal characters")]
    InvalidLength,
    #[error("commit SHA must contain only hexadecimal characters")]
    NonHexadecimal,
}

fn normalize_commit_sha(commit_sha: &str) -> Result<String, CommitShaValidationError> {
    if !(7..=40).contains(&commit_sha.len()) {
        return Err(CommitShaValidationError::InvalidLength);
    }
    if !commit_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CommitShaValidationError::NonHexadecimal);
    }
    Ok(commit_sha.to_ascii_lowercase())
}

fn commit_lookup_principal(auth: &AuthContext) -> String {
    match &auth.source {
        AuthSource::Session { user } | AuthSource::CliToken { user } => {
            format!("user:{}", user.id)
        }
        AuthSource::ApiKey { key_id, .. } => format!("api-key:{}", key_id),
        AuthSource::DeploymentToken { token_id, .. } => {
            format!("deployment-token:{}", token_id)
        }
    }
}

/// Get repository branches
#[utoipa::path(
    get,
    path = "repositories/{owner}/{repo}/branches",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
        ConnectionQueryParams
    ),
    responses(
        (status = 200, description = "List of branches", body = BranchListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Repositories",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_repository_branches(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<ConnectionQueryParams>,
) -> Result<Json<BranchListResponse>, Problem> {
    // Check permission
    permission_check!(auth, Permission::GitRepositoriesRead);

    // Find the repository with the specific connection ID
    state
        .git_provider_manager
        .get_repository_by_owner_and_name_in_connection(&owner, &repo, params.connection_id)
        .await?;

    // We already filtered by connection_id, so we know it exists
    let connection_id = params.connection_id;

    // Get the connection and provider
    let connection = state
        .git_provider_manager
        .get_connection(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider connection")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let provider_service = state
        .git_provider_manager
        .get_provider_service(connection.provider_id)
        .await?;

    let access_token = state
        .git_provider_manager
        .get_connection_token(connection_id)
        .await?;

    // Create cache key
    let cache_key =
        crate::services::cache::BranchCacheKey::new(connection_id, owner.clone(), repo.clone());

    // Try cache first (unless fresh=true)
    if !params.fresh {
        if let Some(cached_branches) = state.cache_manager.branches.get(&cache_key).await {
            let branch_infos: Vec<BranchInfo> = cached_branches
                .into_iter()
                .map(|branch| BranchInfo {
                    name: branch.name,
                    commit_sha: branch.commit_sha,
                    protected: branch.protected,
                })
                .collect();
            return Ok(Json(BranchListResponse {
                branches: branch_infos,
            }));
        }
    }

    // Get branches from the git provider
    let branches = provider_service
        .list_branches(&access_token, &owner, &repo)
        .await?;

    // Cache the result
    state
        .cache_manager
        .branches
        .set(cache_key, branches.clone())
        .await;

    let branch_infos: Vec<BranchInfo> = branches
        .into_iter()
        .map(|branch| BranchInfo {
            name: branch.name,
            commit_sha: branch.commit_sha,
            protected: branch.protected,
        })
        .collect();

    Ok(Json(BranchListResponse {
        branches: branch_infos,
    }))
}

/// Get repository tags
#[utoipa::path(
    get,
    path = "repositories/{owner}/{repo}/tags",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
        ConnectionQueryParams
    ),
    responses(
        (status = 200, description = "List of tags", body = TagListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Repositories",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_repository_tags(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<ConnectionQueryParams>,
) -> Result<Json<TagListResponse>, Problem> {
    // Check permission
    permission_check!(auth, Permission::GitRepositoriesRead);

    // Find the repository with the specific connection ID
    let repository = state
        .git_provider_manager
        .get_repository_by_owner_and_name_in_connection(&owner, &repo, params.connection_id)
        .await?;

    // Repository always has a connection_id (required field)
    let connection_id = repository.git_provider_connection_id;

    // Get the connection and provider
    let connection = state
        .git_provider_manager
        .get_connection(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider connection")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let provider_service = state
        .git_provider_manager
        .get_provider_service(connection.provider_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider service")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let access_token = state
        .git_provider_manager
        .get_connection_token(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get access token")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    // Create cache key
    let cache_key =
        crate::services::cache::TagCacheKey::new(connection_id, owner.clone(), repo.clone());

    // Try cache first (unless fresh=true)
    if !params.fresh {
        if let Some(cached_tags) = state.cache_manager.tags.get(&cache_key).await {
            let tag_infos: Vec<TagInfo> = cached_tags
                .into_iter()
                .map(|tag| TagInfo {
                    name: tag.name,
                    commit_sha: tag.commit_sha,
                })
                .collect();
            return Ok(Json(TagListResponse { tags: tag_infos }));
        }
    }

    // Get tags from the git provider
    let tags = provider_service
        .list_tags(&access_token, &owner, &repo)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to fetch tags")
                .detail(format!("Error fetching tags from git provider: {}", e))
                .build()
        })?;

    // Cache the result
    state.cache_manager.tags.set(cache_key, tags.clone()).await;

    let tag_infos: Vec<TagInfo> = tags
        .into_iter()
        .map(|tag| TagInfo {
            name: tag.name,
            commit_sha: tag.commit_sha,
        })
        .collect();

    Ok(Json(TagListResponse { tags: tag_infos }))
}

/// Get repository branches by repository ID
#[utoipa::path(
    get,
    path = "repository/{repository_id}/branches",
    params(
        ("repository_id" = i32, Path, description = "Repository ID"),
        FreshQueryParams
    ),
    responses(
        (status = 200, description = "List of branches", body = BranchListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Repositories",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_branches_by_repository_id(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(repository_id): Path<i32>,
    Query(params): Query<FreshQueryParams>,
) -> Result<Json<BranchListResponse>, Problem> {
    // Check permission
    permission_check!(auth, Permission::GitRepositoriesRead);

    // Find the repository by ID
    let repository = state
        .git_provider_manager
        .get_repository_by_id(repository_id)
        .await?;

    // Check if repository has a git provider connection
    let connection_id = repository.git_provider_connection_id;

    // Get the connection and provider
    let connection = state
        .git_provider_manager
        .get_connection(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider connection")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let provider_service = state
        .git_provider_manager
        .get_provider_service(connection.provider_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider service")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let access_token = state
        .git_provider_manager
        .get_connection_token(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get access token")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    // Create cache key
    let cache_key = crate::services::cache::BranchCacheKey::new(
        connection_id,
        repository.owner.clone(),
        repository.name.clone(),
    );

    // Try cache first (unless fresh=true)
    if !params.fresh {
        if let Some(cached_branches) = state.cache_manager.branches.get(&cache_key).await {
            let branch_infos: Vec<BranchInfo> = cached_branches
                .into_iter()
                .map(|branch| BranchInfo {
                    name: branch.name,
                    commit_sha: branch.commit_sha,
                    protected: branch.protected,
                })
                .collect();
            return Ok(Json(BranchListResponse {
                branches: branch_infos,
            }));
        }
    }

    // Get branches from the git provider using owner and repo from repository
    let branches = provider_service
        .list_branches(&access_token, &repository.owner, &repository.name)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to fetch branches")
                .detail(format!("Error fetching branches from git provider: {}", e))
                .build()
        })?;

    // Cache the result
    state
        .cache_manager
        .branches
        .set(cache_key, branches.clone())
        .await;

    let branch_infos: Vec<BranchInfo> = branches
        .into_iter()
        .map(|branch| BranchInfo {
            name: branch.name,
            commit_sha: branch.commit_sha,
            protected: branch.protected,
        })
        .collect();

    Ok(Json(BranchListResponse {
        branches: branch_infos,
    }))
}

/// Get repository tags by repository ID
#[utoipa::path(
    get,
    path = "repository/{repository_id}/tags",
    params(
        ("repository_id" = i32, Path, description = "Repository ID"),
        FreshQueryParams
    ),
    responses(
        (status = 200, description = "List of tags", body = TagListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Repositories",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_tags_by_repository_id(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(repository_id): Path<i32>,
    Query(params): Query<FreshQueryParams>,
) -> Result<Json<TagListResponse>, Problem> {
    // Check permission
    permission_check!(auth, Permission::GitRepositoriesRead);

    // Find the repository by ID
    let repository = state
        .git_provider_manager
        .get_repository_by_id(repository_id)
        .await?;

    // Check if repository has a git provider connection
    let connection_id = repository.git_provider_connection_id;

    // Get the connection and provider
    let connection = state
        .git_provider_manager
        .get_connection(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider connection")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let provider_service = state
        .git_provider_manager
        .get_provider_service(connection.provider_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider service")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let access_token = state
        .git_provider_manager
        .get_connection_token(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get access token")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    // Create cache key
    let cache_key = crate::services::cache::TagCacheKey::new(
        connection_id,
        repository.owner.clone(),
        repository.name.clone(),
    );

    // Try cache first (unless fresh=true)
    if !params.fresh {
        if let Some(cached_tags) = state.cache_manager.tags.get(&cache_key).await {
            let tag_infos: Vec<TagInfo> = cached_tags
                .into_iter()
                .map(|tag| TagInfo {
                    name: tag.name,
                    commit_sha: tag.commit_sha,
                })
                .collect();
            return Ok(Json(TagListResponse { tags: tag_infos }));
        }
    }

    // Get tags from the git provider using owner and repo from repository
    let tags = provider_service
        .list_tags(&access_token, &repository.owner, &repository.name)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to fetch tags")
                .detail(format!("Error fetching tags from git provider: {}", e))
                .build()
        })?;

    // Cache the result
    state.cache_manager.tags.set(cache_key, tags.clone()).await;

    let tag_infos: Vec<TagInfo> = tags
        .into_iter()
        .map(|tag| TagInfo {
            name: tag.name,
            commit_sha: tag.commit_sha,
        })
        .collect();

    Ok(Json(TagListResponse { tags: tag_infos }))
}

/// Check if a commit exists in a repository
#[utoipa::path(
    get,
    path = "repository/{repository_id}/commits/{commit_sha}",
    params(
        ("repository_id" = i32, Path, description = "Repository ID"),
        ("commit_sha" = String, Path, description = "Commit SHA to check")
    ),
    responses(
        (status = 200, description = "Commit existence check result", body = CommitExistsResponse),
        (status = 400, description = "Invalid commit SHA"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found"),
        (status = 429, description = "Commit lookup rate limit exceeded"),
        (status = 500, description = "Internal server error"),
        (status = 502, description = "Git provider request failed")
    ),
    tag = "Repositories",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn check_commit_exists(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((repository_id, commit_sha)): Path<(i32, String)>,
) -> Result<Json<CommitExistsResponse>, Problem> {
    // Check permission
    permission_check!(auth, Permission::GitRepositoriesRead);

    let commit_sha = normalize_commit_sha(&commit_sha).map_err(|error| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid Commit SHA")
            .detail(error.to_string())
            .build()
    })?;

    // Find the repository by ID
    let repository = state
        .git_provider_manager
        .get_repository_by_id(repository_id)
        .await?;

    // Check if repository has a git provider connection
    let connection_id = repository.git_provider_connection_id;

    let cache_key = CommitCacheKey::new(
        connection_id,
        repository.owner.clone(),
        repository.name.clone(),
        commit_sha.clone(),
    );
    if let Some(cached_commit) = state.cache_manager.commits.get(&cache_key).await {
        return Ok(Json(match cached_commit {
            Some(commit) => CommitExistsResponse::found(commit),
            None => CommitExistsResponse::missing(),
        }));
    }

    // Get the connection and provider
    let connection = state
        .git_provider_manager
        .get_connection(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider connection")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let provider_service = state
        .git_provider_manager
        .get_provider_service(connection.provider_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider service")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let access_token = state
        .git_provider_manager
        .get_connection_token(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get access token")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let principal = commit_lookup_principal(&auth);
    state
        .cache_manager
        .commit_lookup_rate_limiter
        .check(&principal)
        .await
        .map_err(|retry_after_seconds| {
            ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
                .title("Commit Lookup Rate Limit Exceeded")
                .detail("Too many uncached commit lookups. Please retry later.")
                .value("retry_after_seconds", retry_after_seconds)
                .build()
        })?;

    let commit_result = provider_service
        .get_commit(
            &access_token,
            &repository.owner,
            &repository.name,
            &commit_sha,
        )
        .await;

    match commit_result {
        Ok(commit) => {
            state
                .cache_manager
                .commits
                .set(cache_key, Some(commit.clone()))
                .await;
            Ok(Json(CommitExistsResponse::found(commit)))
        }
        Err(GitProviderError::CommitNotFound { .. }) => {
            state.cache_manager.commits.set(cache_key, None).await;
            Ok(Json(CommitExistsResponse::missing()))
        }
        Err(error @ GitProviderError::RateLimitExceeded) => Err(error.into()),
        Err(error) => {
            warn!(
                repository_id,
                connection_id,
                owner = %repository.owner,
                repository = %repository.name,
                commit_sha = %commit_sha,
                error = %error,
                "Git provider commit lookup failed"
            );
            Err(ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                .title("Failed to fetch commit details")
                .detail("The git provider could not complete the commit lookup.")
                .build())
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct CommitListQueryParams {
    /// Branch name to list commits for
    pub branch: String,
    /// Number of commits to return (default: 20, max: 100)
    pub per_page: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CommitInfo {
    /// Commit SHA hash
    pub sha: String,
    /// Commit message
    pub message: String,
    /// Author name
    pub author: String,
    /// Author email
    pub author_email: String,
    /// Commit date in ISO 8601 format
    #[schema(value_type = String, format = DateTime, example = "2025-10-12T12:15:47.609192Z")]
    pub date: chrono::DateTime<chrono::Utc>,
}

impl From<crate::services::git_provider::Commit> for CommitInfo {
    fn from(commit: crate::services::git_provider::Commit) -> Self {
        Self {
            sha: commit.sha,
            message: commit.message,
            author: commit.author,
            author_email: commit.author_email,
            date: commit.date,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CommitListResponse {
    pub commits: Vec<CommitInfo>,
}

/// List recent commits for a repository branch
#[utoipa::path(
    get,
    path = "repository/{repository_id}/commits",
    params(
        ("repository_id" = i32, Path, description = "Repository ID"),
        CommitListQueryParams
    ),
    responses(
        (status = 200, description = "List of commits", body = CommitListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Repositories",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_commits_by_repository_id(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(repository_id): Path<i32>,
    Query(params): Query<CommitListQueryParams>,
) -> Result<Json<CommitListResponse>, Problem> {
    // Check permission
    permission_check!(auth, Permission::GitRepositoriesRead);

    // Find the repository by ID
    let repository = state
        .git_provider_manager
        .get_repository_by_id(repository_id)
        .await?;

    // Get the connection and provider
    let connection_id = repository.git_provider_connection_id;

    let connection = state
        .git_provider_manager
        .get_connection(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider connection")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let provider_service = state
        .git_provider_manager
        .get_provider_service(connection.provider_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get git provider service")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let access_token = state
        .git_provider_manager
        .get_connection_token(connection_id)
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get access token")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    let per_page = std::cmp::min(params.per_page.unwrap_or(20), 100);

    // Get commits from the git provider
    let commits = provider_service
        .list_commits(
            &access_token,
            &repository.owner,
            &repository.name,
            &params.branch,
            per_page,
        )
        .await
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to fetch commits")
                .detail(format!("Error fetching commits from git provider: {}", e))
                .build()
        })?;

    let commit_infos: Vec<CommitInfo> = commits
        .into_iter()
        .map(|commit| CommitInfo {
            sha: commit.sha,
            message: commit.message,
            author: commit.author,
            author_email: commit.author_email,
            date: commit.date,
        })
        .collect();

    Ok(Json(CommitListResponse {
        commits: commit_infos,
    }))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_repository_branches,
        get_repository_tags,
        get_branches_by_repository_id,
        get_tags_by_repository_id,
        check_commit_exists,
        list_commits_by_repository_id
    ),
    components(
        schemas(
            BranchInfo,
            BranchListResponse,
            TagInfo,
            TagListResponse,
            CommitExistsResponse,
            CommitInfo,
            CommitListResponse
        )
    ),
    tags(
        (name = "Repositories", description = "Repository management endpoints")
    )
)]
pub struct RepositoriesApiDoc;

#[cfg(test)]
mod tests {
    use super::{normalize_commit_sha, CommitExistsResponse, CommitShaValidationError};
    use crate::services::git_provider::Commit;

    #[test]
    fn missing_commit_response_has_no_commit_metadata() {
        let response = CommitExistsResponse::missing();

        assert!(!response.exists);
        assert!(response.commit_sha.is_none());
        assert!(response.commit.is_none());
    }

    #[test]
    fn found_commit_response_includes_provider_metadata() {
        let response = CommitExistsResponse::found(Commit {
            sha: "0123456789abcdef".to_string(),
            message: "Show commit details".to_string(),
            author: "Temps Contributor".to_string(),
            author_email: "contributor@example.com".to_string(),
            date: chrono::DateTime::UNIX_EPOCH,
        });

        assert!(response.exists);
        assert_eq!(response.commit_sha.as_deref(), Some("0123456789abcdef"));
        let commit = response.commit.as_ref();
        assert_eq!(
            commit.map(|value| value.sha.as_str()),
            Some("0123456789abcdef")
        );
        assert_eq!(
            commit.map(|value| value.message.as_str()),
            Some("Show commit details")
        );
        assert_eq!(
            commit.map(|value| value.author.as_str()),
            Some("Temps Contributor")
        );
        assert_eq!(
            commit.map(|value| value.date),
            Some(chrono::DateTime::UNIX_EPOCH)
        );
    }

    #[test]
    fn commit_sha_validation_normalizes_hex_case() {
        assert_eq!(
            normalize_commit_sha("ABCDEF1234567"),
            Ok("abcdef1234567".to_string())
        );
    }

    #[test]
    fn commit_sha_validation_rejects_short_long_and_non_hex_values() {
        assert_eq!(
            normalize_commit_sha("abcdef"),
            Err(CommitShaValidationError::InvalidLength)
        );
        assert_eq!(
            normalize_commit_sha("01234567890123456789012345678901234567890"),
            Err(CommitShaValidationError::InvalidLength)
        );
        assert_eq!(
            normalize_commit_sha("abcdefg"),
            Err(CommitShaValidationError::NonHexadecimal)
        );
        assert_eq!(
            normalize_commit_sha("abcdef/123456"),
            Err(CommitShaValidationError::NonHexadecimal)
        );
    }
}
