use super::types::GitAppState as AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_check, Permission, RequireAuth};
use temps_core::{error_builder::ErrorBuilder, problemdetails::Problem};
use utoipa::{IntoParams, OpenApi, ToSchema};

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

    // We already filtered by connection_id, so we know it exists
    let connection_id = repository.git_provider_connection_id.ok_or(
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("No git provider connection configured")
            .detail("This repository does not have a git provider connection configured")
            .build(),
    )?;

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
    let connection_id = repository.git_provider_connection_id.ok_or_else(|| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("No git provider configured")
            .detail("This repository does not have a git provider connection configured")
            .build()
    })?;

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
    let connection_id = repository.git_provider_connection_id.ok_or_else(|| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("No git provider configured")
            .detail("This repository does not have a git provider connection configured")
            .build()
    })?;

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
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Internal server error")
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

    // Find the repository by ID
    let repository = state
        .git_provider_manager
        .get_repository_by_id(repository_id)
        .await?;

    // Check if repository has a git provider connection
    let connection_id = repository.git_provider_connection_id.ok_or_else(|| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("No git provider configured")
            .detail("This repository does not have a git provider connection configured")
            .build()
    })?;

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

    // Check if commit exists using the git provider
    let exists = provider_service
        .check_commit_exists(
            &access_token,
            &repository.owner,
            &repository.name,
            &commit_sha,
        )
        .await
        .unwrap_or(false); // If there's an error checking, assume it doesn't exist

    Ok(Json(CommitExistsResponse {
        exists,
        commit_sha: if exists { Some(commit_sha) } else { None },
    }))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_repository_branches,
        get_repository_tags,
        get_branches_by_repository_id,
        get_tags_by_repository_id,
        check_commit_exists
    ),
    components(
        schemas(
            BranchInfo,
            BranchListResponse,
            TagInfo,
            TagListResponse,
            CommitExistsResponse
        )
    ),
    tags(
        (name = "Repositories", description = "Repository management endpoints")
    )
)]
pub struct RepositoriesApiDoc;
