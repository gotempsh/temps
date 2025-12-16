//! Public repository endpoints for accessing public repositories without authentication
//!
//! These endpoints allow fetching branches and detecting presets for public repositories
//! without requiring a git provider connection or authentication.
//! Supports multiple providers: GitHub, GitLab, and more in the future.

use super::repositories::{BranchInfo, BranchListResponse};
use super::types::GitAppState as AppState;
use crate::services::cache::{CachedPresetInfo, PublicBranchCacheKey, PublicPresetCacheKey};
use crate::services::git_provider::Branch;
use crate::services::public_repo::{
    detect_presets_from_files, PublicRepoError, PublicRepoProviderFactory,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::problemdetails::{new as problem_new, Problem};
use utoipa::{IntoParams, OpenApi, ToSchema};

/// Query parameters for public repository endpoints
#[derive(Debug, Deserialize, IntoParams)]
pub struct PublicRepoQueryParams {
    /// Force fetch fresh data, bypassing cache (default: false)
    #[serde(default)]
    pub fresh: bool,
}

/// Query parameters for preset detection
#[derive(Debug, Deserialize, IntoParams)]
pub struct PresetQueryParams {
    /// Branch name to detect presets for (default: repository's default branch)
    pub branch: Option<String>,
    /// Force fetch fresh data, bypassing cache (default: false)
    #[serde(default)]
    pub fresh: bool,
}

/// Public repository information
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicRepositoryInfo {
    /// Repository owner
    pub owner: String,
    /// Repository name
    pub name: String,
    /// Full repository name (owner/repo)
    pub full_name: String,
    /// Repository description
    pub description: Option<String>,
    /// Default branch name
    pub default_branch: String,
    /// Primary programming language
    pub language: Option<String>,
    /// Star count
    pub stars: i32,
    /// Fork count
    pub forks: i32,
}

/// Detected preset information
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PresetInfo {
    /// Path where preset was detected (empty for root)
    pub path: String,
    /// Preset slug (e.g., "nextjs", "fastapi")
    pub preset: String,
    /// Human-readable preset label
    pub preset_label: String,
    /// Default exposed port for this preset
    pub exposed_port: Option<i32>,
    /// Icon URL for this preset
    pub icon_url: Option<String>,
    /// Project type (e.g., "frontend", "backend", "fullstack")
    pub project_type: String,
}

/// Response for preset detection
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicPresetResponse {
    /// Branch name where presets were detected
    pub branch: String,
    /// List of detected presets
    pub presets: Vec<PresetInfo>,
}

/// Convert PublicRepoError to Problem
fn map_error(err: PublicRepoError, owner: &str, repo: &str) -> Problem {
    match err {
        PublicRepoError::NotFound(msg) => problem_new(StatusCode::NOT_FOUND)
            .with_title("Repository Not Found")
            .with_detail(format!(
                "Repository {}/{} not found or is not public: {}",
                owner, repo, msg
            )),
        PublicRepoError::RateLimitExceeded => problem_new(StatusCode::TOO_MANY_REQUESTS)
            .with_title("Rate Limit Exceeded")
            .with_detail("API rate limit exceeded for unauthenticated requests. Try again later."),
        PublicRepoError::BranchNotFound(branch) => problem_new(StatusCode::NOT_FOUND)
            .with_title("Branch Not Found")
            .with_detail(format!("Branch '{}' not found in repository", branch)),
        PublicRepoError::ProviderNotSupported(provider) => problem_new(StatusCode::BAD_REQUEST)
            .with_title("Provider Not Supported")
            .with_detail(format!(
                "Provider '{}' is not supported. Supported providers: github, gitlab",
                provider
            )),
        PublicRepoError::ApiError(msg) => problem_new(StatusCode::BAD_GATEWAY)
            .with_title("API Error")
            .with_detail(format!("Failed to fetch data from provider: {}", msg)),
        PublicRepoError::Internal(msg) => problem_new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Internal Error")
            .with_detail(format!("An unexpected error occurred: {}", msg)),
    }
}

/// Get branches for a public repository (supports GitHub and GitLab)
#[utoipa::path(
    get,
    path = "/git/public/{provider}/{owner}/{repo}/branches",
    params(
        ("provider" = String, Path, description = "Git provider (github or gitlab)"),
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
        PublicRepoQueryParams
    ),
    responses(
        (status = 200, description = "List of branches", body = BranchListResponse),
        (status = 400, description = "Provider not supported"),
        (status = 404, description = "Repository not found"),
        (status = 429, description = "API rate limit exceeded"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Public Repositories"
)]
pub async fn get_public_branches(
    State(state): State<Arc<AppState>>,
    Path((provider, owner, repo)): Path<(String, String, String)>,
    Query(params): Query<PublicRepoQueryParams>,
) -> Result<Json<BranchListResponse>, Problem> {
    // Create cache key for public repos
    let cache_key = PublicBranchCacheKey::new(provider.clone(), owner.clone(), repo.clone());

    // Try cache first (unless fresh=true)
    if !params.fresh {
        if let Some(cached_branches) = state.cache_manager.public_branches.get(&cache_key).await {
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

    // Create provider
    let repo_provider =
        PublicRepoProviderFactory::create(&provider).map_err(|e| map_error(e, &owner, &repo))?;

    // Fetch branches from provider
    let provider_branches = repo_provider
        .list_branches(&owner, &repo)
        .await
        .map_err(|e| map_error(e, &owner, &repo))?;

    // Convert to our branch format
    let branches: Vec<Branch> = provider_branches
        .into_iter()
        .map(|b| Branch {
            name: b.name,
            commit_sha: b.commit_sha,
            protected: b.protected,
        })
        .collect();

    // Cache the result
    state
        .cache_manager
        .public_branches
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

/// Detect presets for a public repository (supports GitHub and GitLab)
#[utoipa::path(
    get,
    path = "/git/public/{provider}/{owner}/{repo}/presets",
    params(
        ("provider" = String, Path, description = "Git provider (github or gitlab)"),
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
        PresetQueryParams
    ),
    responses(
        (status = 200, description = "Detected presets", body = PublicPresetResponse),
        (status = 400, description = "Provider not supported"),
        (status = 404, description = "Repository or branch not found"),
        (status = 429, description = "API rate limit exceeded"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Public Repositories"
)]
pub async fn detect_public_presets(
    State(state): State<Arc<AppState>>,
    Path((provider, owner, repo)): Path<(String, String, String)>,
    Query(params): Query<PresetQueryParams>,
) -> Result<Json<PublicPresetResponse>, Problem> {
    // Create provider
    let repo_provider =
        PublicRepoProviderFactory::create(&provider).map_err(|e| map_error(e, &owner, &repo))?;

    // Get repository info to determine default branch if not specified
    let target_branch = if let Some(branch) = params.branch.clone() {
        branch
    } else {
        // Fetch repository info to get default branch
        let repo_info = repo_provider
            .get_repository(&owner, &repo)
            .await
            .map_err(|e| map_error(e, &owner, &repo))?;

        repo_info.default_branch
    };

    // Create cache key
    let cache_key = PublicPresetCacheKey::new(
        provider.clone(),
        owner.clone(),
        repo.clone(),
        target_branch.clone(),
    );

    // Try cache first (unless fresh=true)
    if !params.fresh {
        if let Some(cached_presets) = state.cache_manager.public_presets.get(&cache_key).await {
            // Convert cached presets to response format
            let presets: Vec<PresetInfo> = cached_presets
                .into_iter()
                .map(|p| PresetInfo {
                    path: p.path,
                    preset: p.preset,
                    preset_label: p.preset_label,
                    exposed_port: p.exposed_port,
                    icon_url: p.icon_url,
                    project_type: p.project_type,
                })
                .collect();
            return Ok(Json(PublicPresetResponse {
                branch: target_branch,
                presets,
            }));
        }
    }

    // Fetch file tree from provider
    let files = repo_provider
        .get_file_tree(&owner, &repo, &target_branch)
        .await
        .map_err(|e| map_error(e, &owner, &repo))?;

    // Use centralized preset detection
    let detected = detect_presets_from_files(&files);

    // Convert to CachedPresetInfo for caching
    let cached_presets: Vec<CachedPresetInfo> = detected
        .into_iter()
        .map(|p| CachedPresetInfo {
            path: p.path,
            preset: p.preset,
            preset_label: p.preset_label,
            exposed_port: p.exposed_port,
            icon_url: p.icon_url,
            project_type: p.project_type,
        })
        .collect();

    // Cache the result
    state
        .cache_manager
        .public_presets
        .set(cache_key, cached_presets.clone())
        .await;

    // Convert to response format
    let presets: Vec<PresetInfo> = cached_presets
        .into_iter()
        .map(|p| PresetInfo {
            path: p.path,
            preset: p.preset,
            preset_label: p.preset_label,
            exposed_port: p.exposed_port,
            icon_url: p.icon_url,
            project_type: p.project_type,
        })
        .collect();

    Ok(Json(PublicPresetResponse {
        branch: target_branch,
        presets,
    }))
}

/// Get information about a public repository (supports GitHub and GitLab)
#[utoipa::path(
    get,
    path = "/git/public/{provider}/{owner}/{repo}",
    params(
        ("provider" = String, Path, description = "Git provider (github or gitlab)"),
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Repository information", body = PublicRepositoryInfo),
        (status = 400, description = "Provider not supported"),
        (status = 404, description = "Repository not found"),
        (status = 429, description = "API rate limit exceeded"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Public Repositories"
)]
pub async fn get_public_repository(
    Path((provider, owner, repo)): Path<(String, String, String)>,
) -> Result<Json<PublicRepositoryInfo>, Problem> {
    // Create provider
    let repo_provider =
        PublicRepoProviderFactory::create(&provider).map_err(|e| map_error(e, &owner, &repo))?;

    // Fetch repository info
    let repo_info = repo_provider
        .get_repository(&owner, &repo)
        .await
        .map_err(|e| map_error(e, &owner, &repo))?;

    Ok(Json(PublicRepositoryInfo {
        owner: repo_info.owner,
        name: repo_info.name,
        full_name: repo_info.full_name,
        description: repo_info.description,
        default_branch: repo_info.default_branch,
        language: repo_info.language,
        stars: repo_info.stars,
        forks: repo_info.forks,
    }))
}

/// Configure public repository routes
/// These routes are nested under /git in the main router, so they become /git/public/...
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Generic provider routes: /git/public/{provider}/{owner}/{repo}
        .route(
            "/public/{provider}/{owner}/{repo}",
            axum::routing::get(get_public_repository),
        )
        .route(
            "/public/{provider}/{owner}/{repo}/branches",
            axum::routing::get(get_public_branches),
        )
        .route(
            "/public/{provider}/{owner}/{repo}/presets",
            axum::routing::get(detect_public_presets),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_public_repository,
        get_public_branches,
        detect_public_presets
    ),
    components(
        schemas(
            PublicRepositoryInfo,
            PresetInfo,
            PublicPresetResponse,
            BranchInfo,
            BranchListResponse
        )
    ),
    tags(
        (name = "Public Repositories", description = "Endpoints for accessing public repositories without authentication. Supports GitHub and GitLab.")
    )
)]
pub struct PublicRepositoriesApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cache::GitProviderCacheManager;
    use crate::services::public_repo::{
        GitHubPublicProvider, GitLabPublicProvider, PublicRepoProvider,
    };

    // =============================================================================
    // Unit Tests - Cache Key Tests
    // =============================================================================

    #[test]
    fn test_public_branch_cache_key_equality() {
        let key1 = PublicBranchCacheKey::new(
            "github".to_string(),
            "owner".to_string(),
            "repo".to_string(),
        );
        let key2 = PublicBranchCacheKey::new(
            "github".to_string(),
            "owner".to_string(),
            "repo".to_string(),
        );
        let key3 = PublicBranchCacheKey::new(
            "github".to_string(),
            "other".to_string(),
            "repo".to_string(),
        );

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_public_branch_cache_key_different_providers() {
        let github_key = PublicBranchCacheKey::new(
            "github".to_string(),
            "owner".to_string(),
            "repo".to_string(),
        );
        let gitlab_key = PublicBranchCacheKey::new(
            "gitlab".to_string(),
            "owner".to_string(),
            "repo".to_string(),
        );

        assert_ne!(github_key, gitlab_key);
    }

    #[test]
    fn test_public_preset_cache_key_equality() {
        let key1 = PublicPresetCacheKey::new(
            "github".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            "main".to_string(),
        );
        let key2 = PublicPresetCacheKey::new(
            "github".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            "main".to_string(),
        );
        let key3 = PublicPresetCacheKey::new(
            "github".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            "develop".to_string(),
        );

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_public_preset_cache_key_different_branches() {
        let main_key = PublicPresetCacheKey::new(
            "github".to_string(),
            "facebook".to_string(),
            "react".to_string(),
            "main".to_string(),
        );
        let dev_key = PublicPresetCacheKey::new(
            "github".to_string(),
            "facebook".to_string(),
            "react".to_string(),
            "dev".to_string(),
        );

        assert_ne!(main_key, dev_key);
    }

    // =============================================================================
    // Unit Tests - Cache Operations
    // =============================================================================

    #[tokio::test]
    async fn test_public_branch_cache_set_and_get() {
        let cache_manager = GitProviderCacheManager::new();
        let cache_key = PublicBranchCacheKey::new(
            "github".to_string(),
            "rust-lang".to_string(),
            "rust".to_string(),
        );

        let branches = vec![
            Branch {
                name: "master".to_string(),
                commit_sha: "abc123".to_string(),
                protected: true,
            },
            Branch {
                name: "beta".to_string(),
                commit_sha: "def456".to_string(),
                protected: false,
            },
        ];

        // Set cache
        cache_manager
            .public_branches
            .set(cache_key.clone(), branches.clone())
            .await;

        // Get from cache
        let cached = cache_manager.public_branches.get(&cache_key).await;
        assert!(cached.is_some());
        let cached_branches = cached.unwrap();
        assert_eq!(cached_branches.len(), 2);
        assert_eq!(cached_branches[0].name, "master");
        assert_eq!(cached_branches[1].name, "beta");
    }

    #[tokio::test]
    async fn test_public_preset_cache_set_and_get() {
        let cache_manager = GitProviderCacheManager::new();
        let cache_key = PublicPresetCacheKey::new(
            "github".to_string(),
            "vercel".to_string(),
            "next.js".to_string(),
            "canary".to_string(),
        );

        let presets = vec![CachedPresetInfo {
            path: "".to_string(),
            preset: "nextjs".to_string(),
            preset_label: "Next.js".to_string(),
            exposed_port: Some(3000),
            icon_url: Some("https://example.com/nextjs.svg".to_string()),
            project_type: "frontend".to_string(),
        }];

        // Set cache
        cache_manager
            .public_presets
            .set(cache_key.clone(), presets.clone())
            .await;

        // Get from cache
        let cached = cache_manager.public_presets.get(&cache_key).await;
        assert!(cached.is_some());
        let cached_presets = cached.unwrap();
        assert_eq!(cached_presets.len(), 1);
        assert_eq!(cached_presets[0].preset, "nextjs");
        assert_eq!(cached_presets[0].exposed_port, Some(3000));
    }

    #[tokio::test]
    async fn test_cache_miss_for_different_key() {
        let cache_manager = GitProviderCacheManager::new();
        let cache_key = PublicBranchCacheKey::new(
            "github".to_string(),
            "owner".to_string(),
            "repo".to_string(),
        );

        let branches = vec![Branch {
            name: "main".to_string(),
            commit_sha: "abc123".to_string(),
            protected: false,
        }];

        cache_manager.public_branches.set(cache_key, branches).await;

        // Try to get with a different key
        let different_key = PublicBranchCacheKey::new(
            "github".to_string(),
            "different_owner".to_string(),
            "repo".to_string(),
        );
        let cached = cache_manager.public_branches.get(&different_key).await;
        assert!(cached.is_none());
    }

    #[tokio::test]
    async fn test_cache_invalidation() {
        let cache_manager = GitProviderCacheManager::new();
        let cache_key = PublicBranchCacheKey::new(
            "github".to_string(),
            "owner".to_string(),
            "repo".to_string(),
        );

        let branches = vec![Branch {
            name: "main".to_string(),
            commit_sha: "abc123".to_string(),
            protected: false,
        }];

        cache_manager
            .public_branches
            .set(cache_key.clone(), branches)
            .await;

        // Invalidate
        cache_manager.public_branches.invalidate(&cache_key).await;

        // Should be None now
        let cached = cache_manager.public_branches.get(&cache_key).await;
        assert!(cached.is_none());
    }

    // =============================================================================
    // Unit Tests - Response Type Conversions
    // =============================================================================

    #[test]
    fn test_preset_info_serialization() {
        let preset = PresetInfo {
            path: "apps/web".to_string(),
            preset: "nextjs".to_string(),
            preset_label: "Next.js".to_string(),
            exposed_port: Some(3000),
            icon_url: Some("https://example.com/nextjs.svg".to_string()),
            project_type: "frontend".to_string(),
        };

        let json = serde_json::to_string(&preset).unwrap();
        assert!(json.contains("\"preset\":\"nextjs\""));
        assert!(json.contains("\"exposed_port\":3000"));
        assert!(json.contains("\"path\":\"apps/web\""));
    }

    #[test]
    fn test_public_preset_response_serialization() {
        let response = PublicPresetResponse {
            branch: "main".to_string(),
            presets: vec![
                PresetInfo {
                    path: "".to_string(),
                    preset: "nodejs".to_string(),
                    preset_label: "Node.js".to_string(),
                    exposed_port: Some(3000),
                    icon_url: None,
                    project_type: "backend".to_string(),
                },
                PresetInfo {
                    path: "frontend".to_string(),
                    preset: "react".to_string(),
                    preset_label: "React".to_string(),
                    exposed_port: Some(3000),
                    icon_url: None,
                    project_type: "frontend".to_string(),
                },
            ],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"branch\":\"main\""));
        assert!(json.contains("\"nodejs\""));
        assert!(json.contains("\"react\""));
    }

    #[test]
    fn test_public_repository_info_serialization() {
        let info = PublicRepositoryInfo {
            owner: "facebook".to_string(),
            name: "react".to_string(),
            full_name: "facebook/react".to_string(),
            description: Some(
                "A declarative, efficient, and flexible JavaScript library".to_string(),
            ),
            default_branch: "main".to_string(),
            language: Some("JavaScript".to_string()),
            stars: 200000,
            forks: 40000,
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"owner\":\"facebook\""));
        assert!(json.contains("\"name\":\"react\""));
        assert!(json.contains("\"stars\":200000"));
    }

    // =============================================================================
    // Unit Tests - Error Mapping
    // =============================================================================

    #[test]
    fn test_error_mapping_not_found() {
        let err = PublicRepoError::NotFound("not found".to_string());
        let problem = map_error(err, "owner", "repo");
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_error_mapping_rate_limit() {
        let err = PublicRepoError::RateLimitExceeded;
        let problem = map_error(err, "owner", "repo");
        assert_eq!(problem.status_code, StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_error_mapping_provider_not_supported() {
        let err = PublicRepoError::ProviderNotSupported("bitbucket".to_string());
        let problem = map_error(err, "owner", "repo");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    // =============================================================================
    // Unit Tests - Provider Factory
    // =============================================================================

    #[test]
    fn test_provider_factory_github() {
        let provider = PublicRepoProviderFactory::create("github");
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "github");
    }

    #[test]
    fn test_provider_factory_gitlab() {
        let provider = PublicRepoProviderFactory::create("gitlab");
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "gitlab");
    }

    #[test]
    fn test_provider_factory_case_insensitive() {
        assert!(PublicRepoProviderFactory::create("GitHub").is_ok());
        assert!(PublicRepoProviderFactory::create("GITLAB").is_ok());
        assert!(PublicRepoProviderFactory::create("GiThUb").is_ok());
    }

    #[test]
    fn test_provider_factory_unsupported() {
        let result = PublicRepoProviderFactory::create("bitbucket");
        assert!(result.is_err());
    }

    // =============================================================================
    // Integration Tests - GitHub API (requires network)
    // =============================================================================

    #[tokio::test]
    async fn test_github_provider_get_repository() {
        let provider = GitHubPublicProvider::new();

        match provider.get_repository("expressjs", "express").await {
            Ok(repo) => {
                assert_eq!(repo.name, "express");
                assert!(!repo.full_name.is_empty());
                assert!(repo.stars > 1000, "Express should have many stars");
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_github_provider_list_branches() {
        let provider = GitHubPublicProvider::new();

        match provider.list_branches("expressjs", "express").await {
            Ok(branches) => {
                assert!(!branches.is_empty());
                let has_master = branches.iter().any(|b| b.name == "master");
                assert!(has_master, "Express should have a master branch");
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_github_provider_get_file_tree() {
        let provider = GitHubPublicProvider::new();

        match provider
            .get_file_tree("expressjs", "express", "master")
            .await
        {
            Ok(files) => {
                assert!(!files.is_empty());
                let has_package_json = files.iter().any(|f| f == "package.json");
                assert!(has_package_json, "Express should have package.json");
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_github_provider_nonexistent_repo() {
        let provider = GitHubPublicProvider::new();

        let result = provider
            .get_repository("this-does-not-exist-12345", "fake-repo")
            .await;

        match result {
            Err(PublicRepoError::NotFound(_)) => {
                // Expected
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Ok(_) => panic!("Should have failed for non-existent repo"),
            Err(e) => panic!("Expected NotFound error, got: {:?}", e),
        }
    }

    // =============================================================================
    // Integration Tests - GitLab API (requires network)
    // =============================================================================

    #[tokio::test]
    async fn test_gitlab_provider_get_repository() {
        let provider = GitLabPublicProvider::new(None);

        // Using gitlab-org/gitlab as a well-known public repo
        match provider.get_repository("gitlab-org", "gitlab").await {
            Ok(repo) => {
                assert_eq!(repo.name, "GitLab");
                assert!(!repo.full_name.is_empty());
                assert!(repo.stars > 1000, "GitLab should have many stars");
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_gitlab_provider_list_branches() {
        let provider = GitLabPublicProvider::new(None);

        // Using a smaller public GitLab repo for faster testing
        match provider.list_branches("gitlab-org", "gitlab-runner").await {
            Ok(branches) => {
                assert!(!branches.is_empty(), "GitLab Runner should have branches");
                // Verify branch structure - GitLab runner uses stable branches
                let first_branch = &branches[0];
                assert!(
                    !first_branch.name.is_empty(),
                    "Branch name should not be empty"
                );
                assert!(
                    !first_branch.commit_sha.is_empty(),
                    "Commit SHA should not be empty"
                );
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_gitlab_provider_nonexistent_repo() {
        let provider = GitLabPublicProvider::new(None);

        let result = provider
            .get_repository("this-does-not-exist-12345", "fake-repo")
            .await;

        match result {
            Err(PublicRepoError::NotFound(_)) => {
                // Expected
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Ok(_) => panic!("Should have failed for non-existent repo"),
            Err(e) => panic!("Expected NotFound error, got: {:?}", e),
        }
    }

    // =============================================================================
    // Integration Tests - Preset Detection
    // =============================================================================

    #[tokio::test]
    async fn test_preset_detection_with_github() {
        let provider = GitHubPublicProvider::new();

        match provider.get_file_tree("vercel", "next.js", "canary").await {
            Ok(files) => {
                let presets = detect_presets_from_files(&files);
                let has_nextjs = presets.iter().any(|p| p.preset.contains("next"));
                assert!(has_nextjs, "Should detect Next.js preset");
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    // =============================================================================
    // Integration Tests - Cache with Real Data
    // =============================================================================

    #[tokio::test]
    async fn test_cache_with_real_branch_data() {
        let cache_manager = GitProviderCacheManager::new();
        let provider = GitHubPublicProvider::new();

        // Fetch real branches
        match provider.list_branches("expressjs", "express").await {
            Ok(provider_branches) => {
                let branches: Vec<Branch> = provider_branches
                    .into_iter()
                    .map(|b| Branch {
                        name: b.name,
                        commit_sha: b.commit_sha,
                        protected: b.protected,
                    })
                    .collect();

                let cache_key = PublicBranchCacheKey::new(
                    "github".to_string(),
                    "expressjs".to_string(),
                    "express".to_string(),
                );

                // Cache the branches
                cache_manager
                    .public_branches
                    .set(cache_key.clone(), branches.clone())
                    .await;

                // Verify cache retrieval
                let cached = cache_manager.public_branches.get(&cache_key).await;
                assert!(cached.is_some());

                let cached_branches = cached.unwrap();
                assert_eq!(cached_branches.len(), branches.len());

                // Verify master branch is in cache
                let has_master = cached_branches.iter().any(|b| b.name == "master");
                assert!(has_master, "Cache should contain master branch");
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit.");
            }
            Err(e) => {
                let error_str = e.to_string();
                if error_str.contains("rate limit") || error_str.contains("403") {
                    eprintln!("Skipping test due to GitHub rate limit");
                } else {
                    panic!("Unexpected error: {}", e);
                }
            }
        }
    }
}
