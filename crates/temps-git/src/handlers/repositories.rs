// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    git_provider::{BranchList, Commit, GitProviderError},
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
    /// Whether this is the repository's default branch, as reported by the
    /// git provider (e.g. `main` or `master`). Clients should use this
    /// instead of guessing from the branch name.
    pub is_default: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BranchListResponse {
    pub branches: Vec<BranchInfo>,
    /// Whether the git provider's safety cap stopped the listing before the
    /// repository ran out of branches. When `true`, `branches` is a prefix of
    /// the repository's branches and a name missing from it may still exist:
    /// clients must verify such a name with the single-branch lookup
    /// (`GET repositories/{owner}/{repo}/branch`) instead of reporting it
    /// missing.
    pub truncated: bool,
}

impl BranchListResponse {
    /// Build the response from a provider listing, marking the repository's
    /// default branch as the provider reports it.
    pub(super) fn from_branch_list(list: BranchList, default_branch: &str) -> Self {
        Self {
            branches: list
                .branches
                .into_iter()
                .map(|branch| BranchInfo {
                    is_default: branch.name == default_branch,
                    name: branch.name,
                    commit_sha: branch.commit_sha,
                    protected: branch.protected,
                })
                .collect(),
            truncated: list.truncated,
        }
    }
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

fn provider_lookup_principal(auth: &AuthContext) -> String {
    match &auth.source {
        AuthSource::Session { user, .. } | AuthSource::CliToken { user } => {
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
        (status = 403, description = "Git provider permission required"),
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
    let repository = state
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

    let repository_id = repository.id;

    // Create cache key
    let cache_key =
        crate::services::cache::BranchCacheKey::new(connection_id, owner.clone(), repo.clone());

    // Try cache first (unless fresh=true). A cache hit is the fast path and
    // must stay that way: use our DB-synced default_branch rather than
    // paying for a live provider round-trip (with its own retry/backoff)
    // just to compute is_default on data we're about to return anyway.
    if !params.fresh {
        if let Some(cached) = state.cache_manager.branches.get(&cache_key).await {
            return Ok(Json(BranchListResponse::from_branch_list(
                cached,
                &repository.default_branch,
            )));
        }
    }

    // Cache missed (or fresh=true), so we're making a live provider call
    // either way: also prefer the provider's live default branch here (it
    // can change between syncs), but never let that lookup block branch
    // listing on its own -- fall back to our DB-synced copy so a provider
    // outage still returns branches instead of failing the whole request.
    let default_branch = match provider_service
        .get_repository(&access_token, &owner, &repo)
        .await
    {
        Ok(info) => {
            if let Err(e) = state
                .repository_service
                .update_default_branch(repository_id, &info.default_branch)
                .await
            {
                warn!(
                    "Failed to persist refreshed default branch for repository {}: {}",
                    repository_id, e
                );
            }
            info.default_branch
        }
        Err(_) => repository.default_branch,
    };

    // Get branches from the git provider
    let branches = provider_service
        .list_branches(&access_token, &owner, &repo)
        .await?;

    // Cache the result, truncation flag included: a cache hit must not look
    // more complete than the listing it stored.
    state
        .cache_manager
        .branches
        .set(cache_key, branches.clone())
        .await;

    Ok(Json(BranchListResponse::from_branch_list(
        branches,
        &default_branch,
    )))
}

/// A branch name to look up exactly. Query rather than path, because branch
/// names contain `/` (`release/2026-01`) and a path segment cannot.
#[derive(Debug, Deserialize, IntoParams)]
pub struct BranchLookupQueryParams {
    /// Git provider connection ID (required when multiple connections have the same repo)
    pub connection_id: i32,
    /// Exact branch name to look up
    pub name: String,
}

/// Look up one branch of a repository by exact name
///
/// Answers with one provider request, so it is exact for repositories with
/// more branches than `GET repositories/{owner}/{repo}/branches` returns.
/// A 404 here is the only sound basis for telling a user a branch does not
/// exist.
#[utoipa::path(
    get,
    path = "repositories/{owner}/{repo}/branch",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
        BranchLookupQueryParams
    ),
    responses(
        (status = 200, description = "The branch", body = BranchInfo),
        (status = 400, description = "Empty branch name"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository or branch not found"),
        (status = 429, description = "Branch lookup rate limit exceeded"),
        (status = 500, description = "Internal server error"),
        (status = 502, description = "Git provider request failed")
    ),
    tag = "Repositories",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_repository_branch(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<BranchLookupQueryParams>,
) -> Result<Json<BranchInfo>, Problem> {
    permission_check!(auth, Permission::GitRepositoriesRead);

    let branch_name = params.name.trim().to_string();
    if branch_name.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid Branch Name")
            .detail("The branch name to look up must not be empty.")
            .build());
    }

    let repository = state
        .git_provider_manager
        .get_repository_by_owner_and_name_in_connection(&owner, &repo, params.connection_id)
        .await?;

    let connection_id = params.connection_id;

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

    // One provider request per lookup, so it carries the same per-principal
    // budget as the other uncached provider lookups.
    let principal = provider_lookup_principal(&auth);
    state
        .cache_manager
        .commit_lookup_rate_limiter
        .check(&principal)
        .await
        .map_err(|retry_after_seconds| {
            ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
                .title("Branch Lookup Rate Limit Exceeded")
                .detail("Too many branch lookups. Please retry later.")
                .value("retry_after_seconds", retry_after_seconds)
                .build()
        })?;

    let found = provider_service
        .get_branch(&access_token, &owner, &repo, &branch_name)
        .await;

    match found {
        Ok(Some(branch)) => {
            // Report is_default from the provider's live default branch, as
            // the list handler does, falling back to our DB-synced copy when
            // the provider cannot be reached for it.
            let default_branch = match provider_service
                .get_repository(&access_token, &owner, &repo)
                .await
            {
                Ok(info) => info.default_branch,
                Err(_) => repository.default_branch.clone(),
            };
            Ok(Json(BranchInfo {
                is_default: branch.name == default_branch,
                name: branch.name,
                commit_sha: branch.commit_sha,
                protected: branch.protected,
            }))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Branch Not Found")
            .detail(format!(
                "Repository {}/{} has no branch named '{}'.",
                owner, repo, branch_name
            ))
            .value("branch", branch_name)
            .build()),
        Err(error @ GitProviderError::RateLimitExceeded)
        | Err(error @ GitProviderError::PermissionDenied { .. })
        | Err(error @ GitProviderError::AuthenticationFailed(_)) => Err(error.into()),
        Err(error) => {
            warn!(
                repository_id = repository.id,
                connection_id,
                owner = %owner,
                repository = %repo,
                branch = %branch_name,
                error = %error,
                "Git provider branch lookup failed"
            );
            Err(ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                .title("Failed to look up branch")
                .detail("The git provider could not complete the branch lookup.")
                .build())
        }
    }
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
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository not found"),
        (status = 429, description = "Fresh tag lookup rate limit exceeded"),
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

    if params.fresh {
        let principal = provider_lookup_principal(&auth);
        state
            .cache_manager
            .tag_refresh_rate_limiter
            .check(&principal)
            .await
            .map_err(|retry_after_seconds| {
                ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
                    .title("Tag Refresh Rate Limit Exceeded")
                    .detail("Too many fresh tag lookups. Please retry later.")
                    .value("retry_after_seconds", retry_after_seconds)
                    .build()
            })?;
    }

    // Get tags from the git provider
    let tags = provider_service
        .list_tags(&access_token, &owner, &repo)
        .await
        .map_err(Problem::from)?;

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
        (status = 403, description = "Git provider permission required"),
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

    // Try cache first (unless fresh=true). A cache hit is the fast path and
    // must stay that way: use our DB-synced default_branch rather than
    // paying for a live provider round-trip (with its own retry/backoff)
    // just to compute is_default on data we're about to return anyway.
    if !params.fresh {
        if let Some(cached) = state.cache_manager.branches.get(&cache_key).await {
            return Ok(Json(BranchListResponse::from_branch_list(
                cached,
                &repository.default_branch,
            )));
        }
    }

    // Cache missed (or fresh=true), so we're making a live provider call
    // either way: also prefer the provider's live default branch here (it
    // can change between syncs), but never let that lookup block branch
    // listing on its own -- fall back to our DB-synced copy so a provider
    // outage still returns branches instead of failing the whole request.
    let default_branch = match provider_service
        .get_repository(&access_token, &repository.owner, &repository.name)
        .await
    {
        Ok(info) => {
            if let Err(e) = state
                .repository_service
                .update_default_branch(repository_id, &info.default_branch)
                .await
            {
                warn!(
                    "Failed to persist refreshed default branch for repository {}: {}",
                    repository_id, e
                );
            }
            info.default_branch
        }
        Err(_) => repository.default_branch.clone(),
    };

    // Get branches from the git provider using owner and repo from repository
    let branches = provider_service
        .list_branches(&access_token, &repository.owner, &repository.name)
        .await
        .map_err(Problem::from)?;

    // Cache the result, truncation flag included: a cache hit must not look
    // more complete than the listing it stored.
    state
        .cache_manager
        .branches
        .set(cache_key, branches.clone())
        .await;

    Ok(Json(BranchListResponse::from_branch_list(
        branches,
        &default_branch,
    )))
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
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository not found"),
        (status = 429, description = "Fresh tag lookup rate limit exceeded"),
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

    if params.fresh {
        let principal = provider_lookup_principal(&auth);
        state
            .cache_manager
            .tag_refresh_rate_limiter
            .check(&principal)
            .await
            .map_err(|retry_after_seconds| {
                ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
                    .title("Tag Refresh Rate Limit Exceeded")
                    .detail("Too many fresh tag lookups. Please retry later.")
                    .value("retry_after_seconds", retry_after_seconds)
                    .build()
            })?;
    }

    // Get tags from the git provider using owner and repo from repository
    let tags = provider_service
        .list_tags(&access_token, &repository.owner, &repository.name)
        .await
        .map_err(Problem::from)?;

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

    let principal = provider_lookup_principal(&auth);
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
        Err(error @ GitProviderError::RateLimitExceeded)
        | Err(error @ GitProviderError::PermissionDenied { .. })
        | Err(error @ GitProviderError::AuthenticationFailed(_)) => Err(error.into()),
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
        (status = 403, description = "Git provider permission required"),
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
        .map_err(Problem::from)?;

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
        get_repository_branch,
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

#[cfg(test)]
mod branch_list_response_tests {
    use super::BranchListResponse;
    use crate::services::git_provider::{Branch, BranchList};

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.to_string(),
            commit_sha: "a".repeat(40),
            protected: false,
        }
    }

    #[test]
    fn a_capped_listing_tells_the_client_the_list_is_not_the_whole_set() {
        let response = BranchListResponse::from_branch_list(
            BranchList::truncated(vec![branch("topic")]),
            "main",
        );

        assert!(
            response.truncated,
            "a client that cannot see the cap reports existing branches as missing"
        );
        assert!(
            !response.branches[0].is_default,
            "the default branch is whatever the provider says, never the first row"
        );
    }

    #[test]
    fn a_whole_listing_marks_the_default_branch_and_reports_no_truncation() {
        let response = BranchListResponse::from_branch_list(
            BranchList::complete(vec![branch("topic"), branch("main")]),
            "main",
        );

        assert!(!response.truncated);
        assert_eq!(
            response
                .branches
                .iter()
                .filter(|info| info.is_default)
                .map(|info| info.name.as_str())
                .collect::<Vec<_>>(),
            vec!["main"]
        );
    }
}

#[cfg(test)]
mod branch_lookup_handler_tests {
    //! The exact single-branch endpoint: what a client may conclude from each
    //! status, and that nothing is concluded before authorization.
    use super::*;
    use crate::handlers::public_route_tests::{state, state_over};
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_auth::Role;
    use temps_entities::{git_provider_connections, git_providers, repositories, users};
    use tower::ServiceExt;

    /// The test's provider credential, encrypted with the same key
    /// `public_route_tests` gives the state's `EncryptionService`.
    const ENCRYPTION_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn router() -> Router<Arc<AppState>> {
        Router::new().route(
            "/repositories/{owner}/{repo}/branch",
            get(get_repository_branch),
        )
    }

    fn test_user() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn repository_row() -> repositories::Model {
        let now = chrono::Utc::now();
        repositories::Model {
            id: 5,
            git_provider_connection_id: 7,
            owner: "temps".to_string(),
            name: "example".to_string(),
            full_name: "temps/example".to_string(),
            description: None,
            private: false,
            fork: false,
            created_at: now,
            updated_at: now,
            pushed_at: now,
            size: 0,
            stargazers_count: 0,
            watchers_count: 0,
            language: None,
            default_branch: "main".to_string(),
            open_issues_count: 0,
            topics: String::new(),
            repo_object: String::new(),
            installation_id: None,
            clone_url: None,
            ssh_url: None,
            preset: None,
        }
    }

    fn connection_row() -> git_provider_connections::Model {
        let now = chrono::Utc::now();
        let encrypted = temps_core::EncryptionService::new(ENCRYPTION_KEY)
            .expect("test encryption key")
            .encrypt_string("provider-token")
            .expect("encrypt the stored credential");
        git_provider_connections::Model {
            id: 7,
            provider_id: 3,
            user_id: Some(1),
            account_name: "temps".to_string(),
            account_type: "Organization".to_string(),
            access_token: Some(encrypted),
            refresh_token: None,
            token_expires_at: None,
            refresh_token_expires_at: None,
            installation_id: None,
            metadata: None,
            is_active: true,
            is_expired: false,
            syncing: false,
            last_synced_at: None,
            synced_repository_count: 0,
            health_status: "healthy".to_string(),
            health_message: None,
            last_health_check_at: None,
            consecutive_health_failures: 0,
            last_sync_error: None,
            last_sync_error_at: None,
            credential_generation: 1,
            token_refresh_lease_id: None,
            token_refresh_lease_expires_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn provider_row(api_url: &str) -> git_providers::Model {
        let now = chrono::Utc::now();
        git_providers::Model {
            id: 3,
            name: "GitHub".to_string(),
            provider_type: "github".to_string(),
            base_url: Some("https://github.com".to_string()),
            api_url: Some(api_url.to_string()),
            auth_method: "pat".to_string(),
            auth_config: serde_json::json!({
                "type": "personal_access_token",
                "token": "provider-token"
            }),
            webhook_secret: None,
            is_active: true,
            is_default: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// The rows the handler reads, in the order it reads them: the repository,
    /// the connection, the provider (building the provider service), then the
    /// connection and provider again for the access token.
    fn database(api_url: &str) -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![repository_row()]])
            .append_query_results([vec![connection_row()]])
            .append_query_results([vec![provider_row(api_url)]])
            .append_query_results([vec![connection_row()]])
            .append_query_results([vec![provider_row(api_url)]])
    }

    fn request(auth: Option<AuthContext>) -> Request<Body> {
        let mut builder =
            Request::builder().uri("/repositories/temps/example/branch?connection_id=7&name=main");
        if let Some(auth) = auth {
            builder = builder.extension(auth);
        }
        builder.body(Body::empty()).expect("request builds")
    }

    /// A credential the provider accepts, so `token_needs_refresh` does not
    /// turn a healthy connection into a refresh attempt.
    async fn mock_valid_token(server: &mut mockito::ServerGuard) -> mockito::Mock {
        server
            .mock("GET", "/rate_limit")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("x-ratelimit-remaining", "4999")
            .with_body(r#"{"resources":{"core":{"remaining":4999}}}"#)
            .expect_at_least(1)
            .create_async()
            .await
    }

    #[tokio::test]
    async fn an_unauthenticated_request_is_turned_away_before_any_database_work() {
        let response = router()
            .with_state(state())
            .oneshot(request(None))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_principal_without_the_read_permission_is_refused() {
        let auth = AuthContext::new_api_key(
            test_user(),
            None,
            Some(vec![temps_auth::Permission::ProjectsRead]),
            "scoped-key".to_string(),
            1,
        );

        let response = router()
            .with_state(state())
            .oneshot(request(Some(auth)))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_empty_branch_name_is_rejected_without_calling_the_provider() {
        let response = router()
            .with_state(state())
            .oneshot(
                Request::builder()
                    .uri("/repositories/temps/example/branch?connection_id=7&name=%20")
                    .extension(AuthContext::new_session(test_user(), Role::Admin))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_branch_the_provider_has_is_returned_with_its_commit_and_default_flag() {
        let mut server = mockito::Server::new_async().await;
        let token = mock_valid_token(&mut server).await;
        let branch = server
            .mock("GET", "/repos/temps/example/branches/main")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "name": "main",
                    "commit": { "sha": "a".repeat(40) },
                    "protected": true
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        // `is_default` comes from the provider's live default branch, as it
        // does for the branch list.
        let repository = server
            .mock("GET", "/repos/temps/example")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": 1,
                    "name": "example",
                    "full_name": "temps/example",
                    "private": false,
                    "default_branch": "main",
                    "clone_url": "https://github.com/temps/example.git",
                    "html_url": "https://github.com/temps/example",
                    "owner": { "login": "temps", "id": 1 }
                })
                .to_string(),
            )
            .expect_at_least(1)
            .create_async()
            .await;

        let response = router()
            .with_state(state_over(database(&server.url())))
            .oneshot(request(Some(AuthContext::new_session(
                test_user(),
                Role::Admin,
            ))))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read the response body");
        let found: BranchInfo = serde_json::from_slice(&body).expect("a branch DTO");
        assert_eq!(found.name, "main");
        assert_eq!(found.commit_sha, "a".repeat(40));
        assert!(found.protected);
        assert!(found.is_default);

        token.assert_async().await;
        branch.assert_async().await;
        repository.assert_async().await;
    }

    #[tokio::test]
    async fn a_branch_the_provider_does_not_have_is_a_404() {
        let mut server = mockito::Server::new_async().await;
        let token = mock_valid_token(&mut server).await;
        let branch = server
            .mock("GET", "/repos/temps/example/branches/main")
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"message":"Branch not found"}"#)
            .expect(1)
            .create_async()
            .await;

        let response = router()
            .with_state(state_over(database(&server.url())))
            .oneshot(request(Some(AuthContext::new_session(
                test_user(),
                Role::Admin,
            ))))
            .await
            .expect("router responds");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        token.assert_async().await;
        branch.assert_async().await;
    }
}
