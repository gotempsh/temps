// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    detect_presets_from_files, PublicRepoError, PublicRepoProvider, PublicRepoProviderFactory,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    Extension, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{AuthContext, Permission};
use temps_core::problemdetails::{new as problem_new, Problem};
use temps_entities::preset::ComposePortMapping;
use tracing::warn;
use utoipa::{IntoParams, OpenApi, ToSchema};

const MAX_PUBLIC_DOCKERFILES_TO_SCAN: usize = 1;

/// Query parameters for public repository endpoints
#[derive(Debug, Deserialize, IntoParams)]
pub struct PublicRepoQueryParams {
    /// HTTPS/443 origin of a self-hosted GitLab instance. Requires authentication and git_repositories:read.
    pub base_url: Option<String>,
    /// Force fetch fresh data, bypassing cache (default: false)
    #[serde(default)]
    pub fresh: bool,
}

/// Query parameters for endpoints that do not expose cache controls.
#[derive(Debug, Deserialize, IntoParams)]
pub struct PublicGitLabQueryParams {
    /// HTTPS/443 origin of a self-hosted GitLab instance. Requires authentication and git_repositories:read.
    pub base_url: Option<String>,
}

/// Query parameters for preset detection
#[derive(Debug, Deserialize, IntoParams)]
pub struct PresetQueryParams {
    /// HTTPS/443 origin of a self-hosted GitLab instance. Requires authentication and git_repositories:read.
    pub base_url: Option<String>,
    /// Branch name to detect presets for (default: repository's default branch)
    pub branch: Option<String>,
    /// Force fetch fresh data, bypassing cache (default: false)
    #[serde(default)]
    pub fresh: bool,
}

/// Query parameters for env-example detection.
#[derive(Debug, Deserialize, IntoParams)]
pub struct EnvExampleQueryParams {
    /// HTTPS/443 origin of a self-hosted GitLab instance. Requires authentication and git_repositories:read.
    pub base_url: Option<String>,
    /// Branch name to inspect (default: repository's default branch)
    pub branch: Option<String>,
    /// Project root directory to search (default: repository root)
    pub root_directory: Option<String>,
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
    /// Compose file paths found in the repository (only for docker-compose preset)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_files: Option<Vec<String>>,
}

/// Response for preset detection
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicPresetResponse {
    /// Branch name where presets were detected
    pub branch: String,
    /// List of detected presets
    pub presets: Vec<PresetInfo>,
}

/// A single environment variable parsed from a detected env-example file
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct EnvExampleVariable {
    /// Variable name (e.g. "DATABASE_URL")
    pub key: String,
    /// Placeholder/default value as written in the file (may be empty)
    pub default_value: String,
    /// Description derived from a `# comment` immediately preceding the
    /// variable in the file, if any
    pub description: Option<String>,
}

/// Response for env-example detection
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicEnvExampleResponse {
    /// Branch the file was read from
    pub branch: String,
    /// Path of the detected env-example file (e.g. ".env.example"), `null`
    /// if the repository has none
    pub path: Option<String>,
    /// Parsed variables (empty if no env-example file was found)
    pub variables: Vec<EnvExampleVariable>,
}

/// Query params for public compose-file service preview
#[derive(Debug, Deserialize, IntoParams)]
pub struct PublicComposeFileQueryParams {
    /// HTTPS/443 origin of a self-hosted GitLab instance. Requires authentication and git_repositories:read.
    pub base_url: Option<String>,
    /// Branch to read the compose file from (default: repository's default branch)
    pub branch: Option<String>,
    /// Compose file path to fetch and parse (from the `compose_files` list
    /// `/preset` already returned, or a custom path the user typed)
    pub path: String,
}

/// A single service parsed from a compose file's `services:` map
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PublicComposeServicePreview {
    pub name: String,
    pub image: Option<String>,
    pub depends_on: Vec<String>,
    /// Environment variable names declared by this service. Values are
    /// intentionally omitted.
    pub environment_variables: Vec<String>,
    /// True when the image looks like a well-known database engine
    /// (Postgres/MySQL/MariaDB/MongoDB/Redis and common forks) — a raw
    /// compose service never becomes a Temps-managed `external_services` row,
    /// so it never gets backup/restore. Informational only.
    pub looks_like_database: bool,
    /// Well-known managed-service family detected from the image, if any.
    pub detected_service_type: Option<temps_entities::preset::ComposeServiceFamily>,
    /// Ports declared by Compose. `target` is the container port Temps can
    /// route to; `published` is only the optional Docker host port.
    pub ports: Vec<ComposePortMapping>,
}

/// Response for compose-file service preview
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicComposeServicesResponse {
    /// Branch the file was read from
    pub branch: String,
    pub path: String,
    pub services: Vec<PublicComposeServicePreview>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicComposePreviewRequest {
    pub branch: Option<String>,
    pub path: String,
    pub compose_override: Option<String>,
    #[serde(default)]
    pub excluded_services: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicComposePreviewResponse {
    pub branch: String,
    pub path: String,
    /// Effective user-controlled Compose YAML with sensitive values redacted.
    pub effective_compose: String,
    pub enabled_services: Vec<String>,
    pub disabled_services: Vec<String>,
    pub redacted_values: usize,
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
        PublicRepoError::RepositoryNotPublic(_) => problem_new(StatusCode::NOT_FOUND)
            .with_title("Repository Not Found")
            .with_detail(format!(
                "Repository {}/{} was not found or is not public.",
                owner, repo
            )),
        PublicRepoError::RateLimitExceeded => problem_new(StatusCode::TOO_MANY_REQUESTS)
            .with_title("Rate Limit Exceeded")
            .with_detail(
                "The provider API rate limit was reached. When you are signed in, Temps automatically uses a valid GitHub connection owned by your account. Check or reconnect it in Git Providers, retry after the provider resets the limit, or install a GitHub App without creating a personal access token.",
            ),
        PublicRepoError::PermissionDenied {
            operation,
            required_permission,
        } => problem_new(StatusCode::FORBIDDEN)
            .with_title("Git Provider Permission Required")
            .with_detail(format!(
                "The git provider denied permission to {}. Grant '{}' to the credential for this repository.",
                operation, required_permission
            ))
            .with_value("operation", operation)
            .with_value("required_permission", required_permission),
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
        PublicRepoError::ResponseTooLarge {
            context,
            limit_bytes,
        } => problem_new(StatusCode::BAD_GATEWAY)
            .with_title("Git Provider Response Too Large")
            .with_detail(format!(
                "The provider response for {context} exceeded the {limit_bytes}-byte safety limit."
            )),
    }
}

fn require_public_repository(
    info: crate::services::public_repo::PublicRepoInfo,
    owner: &str,
    repo: &str,
) -> Result<crate::services::public_repo::PublicRepoInfo, PublicRepoError> {
    if info.is_private {
        Err(PublicRepoError::NotFound(format!(
            "Repository {owner}/{repo} is private"
        )))
    } else {
        Ok(info)
    }
}

/// Build the provider used by a public-repository request.
///
/// Authenticated GitHub users may contribute their own connection token so
/// discovery receives GitHub's authenticated rate limit. Before that token is
/// used for any repository content, we confirm the target is still public;
/// this keeps private data out of public responses and shared caches.
async fn provider_for_public_request(
    state: &AppState,
    auth: Option<&AuthContext>,
    provider: &str,
    owner: &str,
    repo: &str,
    base_url: Option<&str>,
) -> Result<
    (
        Box<dyn crate::services::public_repo::PublicRepoProvider>,
        crate::services::public_repo::PublicRepoInfo,
        String,
    ),
    Problem,
> {
    let token = if provider.eq_ignore_ascii_case("github") {
        if let Some(user_id) = auth
            .filter(|auth| auth.has_permission(&Permission::GitRepositoriesRead))
            .and_then(AuthContext::user_id_opt)
        {
            state
                .git_provider_manager
                .get_valid_github_token_for_user(user_id)
                .await
        } else {
            None
        }
    } else {
        None
    };
    authorize_custom_gitlab_origin(auth, base_url)?;
    let gitlab_instance = validated_gitlab_instance(provider, base_url).await?;
    let cache_scope = public_cache_scope(
        provider,
        gitlab_instance
            .as_ref()
            .map(|(base_url, _, _)| base_url.as_str()),
    );
    let repo_provider =
        PublicRepoProviderFactory::create_with_gitlab_instance(provider, token, gitlab_instance)
            .map_err(|error| map_error(error, owner, repo))?;

    // Always verify current visibility, including before shared cache hits.
    // A repository can become private while a branch or preset entry remains
    // cached, so cached content is not itself proof that it is still public.
    let info = repo_provider
        .get_repository(owner, repo)
        .await
        .map_err(|error| map_error(error, owner, repo))?;
    let info = require_public_repository(info, owner, repo)
        .map_err(|error| map_error(error, owner, repo))?;
    Ok((repo_provider, info, cache_scope))
}

fn public_cache_scope(provider: &str, gitlab_base_url: Option<&str>) -> String {
    gitlab_base_url
        .map(|base_url| format!("gitlab@{base_url}"))
        .unwrap_or_else(|| provider.to_ascii_lowercase())
}

fn authorize_custom_gitlab_origin(
    auth: Option<&AuthContext>,
    base_url: Option<&str>,
) -> Result<(), Problem> {
    let has_custom_origin = base_url
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if !has_custom_origin {
        return Ok(());
    }

    let auth = auth.ok_or_else(|| {
        problem_new(StatusCode::UNAUTHORIZED)
            .with_title("Authentication Required")
            .with_detail("Authentication is required to access a custom GitLab instance.")
    })?;
    if !auth.has_permission(&Permission::GitRepositoriesRead) {
        return Err(problem_new(StatusCode::FORBIDDEN)
            .with_title("Insufficient Permissions")
            .with_detail(
                "Accessing a custom GitLab instance requires the git_repositories:read permission.",
            ));
    }

    Ok(())
}

fn authorize_public_preset_refresh(auth: Option<&AuthContext>, fresh: bool) -> Result<(), Problem> {
    if !fresh {
        return Ok(());
    }
    let auth = auth.ok_or_else(|| {
        problem_new(StatusCode::UNAUTHORIZED)
            .with_title("Authentication Required")
            .with_detail("Refreshing public repository presets requires authentication.")
    })?;
    if !auth.has_permission(&Permission::GitRepositoriesRead) {
        return Err(problem_new(StatusCode::FORBIDDEN)
            .with_title("Insufficient Permissions")
            .with_detail(
                "Refreshing public repository presets requires the git_repositories:read permission.",
            ));
    }
    Ok(())
}

/// Validate and normalize a user-supplied self-hosted GitLab origin.
///
/// The result contains the normalized origin, hostname, and the exact public
/// addresses the HTTP client must use. Redirects are disabled by the provider.
async fn validated_gitlab_instance(
    provider: &str,
    base_url: Option<&str>,
) -> Result<Option<(String, String, Vec<std::net::SocketAddr>)>, Problem> {
    let Some(base_url) = base_url.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if !provider.eq_ignore_ascii_case("gitlab") {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Git Provider URL")
            .with_detail("A custom base_url can only be used with the GitLab provider."));
    }

    let mut parsed =
        temps_core::url_validation::validate_external_url(base_url).map_err(|error| {
            problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid GitLab Instance URL")
                .with_detail(format!(
                    "The GitLab base_url is not a safe external URL: {error}"
                ))
        })?;
    if parsed.scheme() != "https" {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid GitLab Instance URL")
            .with_detail("The GitLab base_url must use HTTPS."));
    }
    if parsed.port_or_known_default() != Some(443) {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid GitLab Instance URL")
            .with_detail("The GitLab base_url must use the standard HTTPS port 443."));
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid GitLab Instance URL")
            .with_detail(
                "The GitLab base_url must be an HTTPS origin without credentials, a path, a query, or a fragment.",
            ));
    }

    let hostname = parsed.host_str().ok_or_else(|| {
        problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid GitLab Instance URL")
            .with_detail("The GitLab base_url must include a hostname.")
    })?;
    let addresses = temps_core::url_validation::resolve_and_validate_domain(
        hostname,
        parsed.port_or_known_default().unwrap_or(443),
    )
    .await
    .map_err(|error| {
        problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid GitLab Instance URL")
            .with_detail(format!(
                "The GitLab base_url did not resolve exclusively to public addresses: {error}"
            ))
    })?;
    let hostname = hostname.to_string();
    parsed.set_path("");
    Ok(Some((
        parsed.as_str().trim_end_matches('/').to_string(),
        hostname,
        addresses,
    )))
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
        (status = 401, description = "Authentication required for custom GitLab origins"),
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository not found"),
        (status = 429, description = "API rate limit exceeded"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Public Repositories"
)]
pub async fn get_public_branches(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthContext>>,
    Path((provider, owner, repo)): Path<(String, String, String)>,
    Query(params): Query<PublicRepoQueryParams>,
) -> Result<Json<BranchListResponse>, Problem> {
    let (repo_provider, _, cache_scope) = provider_for_public_request(
        state.as_ref(),
        auth.as_ref().map(|Extension(auth)| auth),
        &provider,
        &owner,
        &repo,
        params.base_url.as_deref(),
    )
    .await?;

    // Create cache key for public repos
    let cache_key = PublicBranchCacheKey::new(cache_scope, owner.clone(), repo.clone());

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
        (status = 401, description = "Authentication required for custom GitLab origins"),
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository or branch not found"),
        (status = 429, description = "API rate limit exceeded"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Public Repositories"
)]
pub async fn detect_public_presets(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthContext>>,
    Path((provider, owner, repo)): Path<(String, String, String)>,
    Query(params): Query<PresetQueryParams>,
) -> Result<Json<PublicPresetResponse>, Problem> {
    authorize_public_preset_refresh(auth.as_ref().map(|Extension(auth)| auth), params.fresh)?;

    let (repo_provider, repo_info, cache_scope) = provider_for_public_request(
        state.as_ref(),
        auth.as_ref().map(|Extension(auth)| auth),
        &provider,
        &owner,
        &repo,
        params.base_url.as_deref(),
    )
    .await?;

    // Get repository info to determine default branch if not specified
    let target_branch = if let Some(branch) = params.branch.clone() {
        branch
    } else {
        repo_info.default_branch
    };

    // Create cache key
    let cache_key = PublicPresetCacheKey::new(
        cache_scope,
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
                    compose_files: p.compose_files,
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
    let mut detected = detect_presets_from_files(&files);
    enrich_public_dockerfile_exposed_ports(
        repo_provider.as_ref(),
        &owner,
        &repo,
        &target_branch,
        &mut detected,
    )
    .await;

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
            compose_files: p.compose_files,
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
            compose_files: p.compose_files,
        })
        .collect();

    Ok(Json(PublicPresetResponse {
        branch: target_branch,
        presets,
    }))
}

async fn enrich_public_dockerfile_exposed_ports(
    repo_provider: &dyn PublicRepoProvider,
    owner: &str,
    repository_name: &str,
    target_branch: &str,
    detected: &mut [crate::services::public_repo::DetectedPreset],
) {
    let dockerfiles: Vec<(usize, String)> = detected
        .iter()
        .enumerate()
        .filter(|(_, preset)| preset.preset == "dockerfile")
        .take(MAX_PUBLIC_DOCKERFILES_TO_SCAN)
        .map(|(index, preset)| {
            let path = if preset.path == "./" || preset.path.is_empty() {
                "Dockerfile".to_string()
            } else {
                format!("{}/Dockerfile", preset.path.trim_end_matches('/'))
            };
            (index, path)
        })
        .collect();

    for (preset_index, dockerfile_path) in dockerfiles {
        match repo_provider
            .get_file_content(owner, repository_name, &dockerfile_path, target_branch)
            .await
        {
            Ok(file) => {
                let content = decode_file_content(&file.content, &file.encoding);
                detected[preset_index].exposed_port =
                    temps_presets::detect_primary_exposed_port(&content).map(i32::from);
            }
            Err(error) => warn!(
                owner,
                repository = repository_name,
                branch = target_branch,
                dockerfile = dockerfile_path,
                error = %error,
                "Could not inspect public Dockerfile EXPOSE metadata during preset detection"
            ),
        }
    }
}

/// Decode a provider file's content. GitHub and GitLab both return base64
/// (with embedded newlines in GitHub's case); falls back to the raw string
/// if decoding fails or the encoding isn't base64.
fn decode_file_content(content: &str, encoding: &str) -> String {
    use base64::Engine;
    if encoding.eq_ignore_ascii_case("base64") {
        let stripped: String = content.split_whitespace().collect();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(stripped) {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    content.to_string()
}

/// Detect and parse a `.env.example`-style file for a public repository
/// (supports GitHub and GitLab)
#[utoipa::path(
    get,
    path = "/git/public/{provider}/{owner}/{repo}/env-example",
    params(
        ("provider" = String, Path, description = "Git provider (github or gitlab)"),
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
        EnvExampleQueryParams
    ),
    responses(
        (status = 200, description = "Detected env-example variables", body = PublicEnvExampleResponse),
        (status = 400, description = "Provider not supported"),
        (status = 401, description = "Authentication required for custom GitLab origins"),
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository or branch not found"),
        (status = 429, description = "API rate limit exceeded"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Public Repositories"
)]
pub async fn detect_public_env_example(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthContext>>,
    Path((provider, owner, repo)): Path<(String, String, String)>,
    Query(params): Query<EnvExampleQueryParams>,
) -> Result<Json<PublicEnvExampleResponse>, Problem> {
    let (repo_provider, repo_info, _) = provider_for_public_request(
        state.as_ref(),
        auth.as_ref().map(|Extension(auth)| auth),
        &provider,
        &owner,
        &repo,
        params.base_url.as_deref(),
    )
    .await?;

    let target_branch = if let Some(branch) = params.branch.clone() {
        branch
    } else {
        repo_info.default_branch
    };

    let files = repo_provider
        .get_file_tree(&owner, &repo, &target_branch)
        .await
        .map_err(|e| map_error(e, &owner, &repo))?;

    let Some(env_path) = temps_presets::detect_env_example_files_in_directory(
        &files,
        params.root_directory.as_deref().unwrap_or("./"),
    )
    .into_iter()
    .next() else {
        return Ok(Json(PublicEnvExampleResponse {
            branch: target_branch,
            path: None,
            variables: Vec::new(),
        }));
    };

    let file = repo_provider
        .get_file_content(&owner, &repo, &env_path, &target_branch)
        .await
        .map_err(|e| map_error(e, &owner, &repo))?;

    let content = decode_file_content(&file.content, &file.encoding);
    let variables = temps_presets::parse_env_example(&content)
        .into_iter()
        .map(|v| EnvExampleVariable {
            key: v.key,
            default_value: v.default_value,
            description: v.description,
        })
        .collect();

    Ok(Json(PublicEnvExampleResponse {
        branch: target_branch,
        path: Some(env_path),
        variables,
    }))
}

/// Parse a compose file's services for a public repository (supports GitHub
/// and GitLab). Unlike env-example detection, the caller already knows the
/// path (from the `compose_files` list `/preset` already returned), so this
/// fetches that one file directly rather than scanning the tree first.
#[utoipa::path(
    get,
    path = "/git/public/{provider}/{owner}/{repo}/compose-file",
    params(
        ("provider" = String, Path, description = "Git provider (github or gitlab)"),
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
        PublicComposeFileQueryParams
    ),
    responses(
        (status = 200, description = "Compose services parsed successfully", body = PublicComposeServicesResponse),
        (status = 400, description = "Provider not supported, or the compose file could not be parsed"),
        (status = 401, description = "Authentication required for custom GitLab origins"),
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository, branch, or compose file not found"),
        (status = 429, description = "API rate limit exceeded"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Public Repositories"
)]
pub async fn get_public_compose_services(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthContext>>,
    Path((provider, owner, repo)): Path<(String, String, String)>,
    Query(params): Query<PublicComposeFileQueryParams>,
) -> Result<Json<PublicComposeServicesResponse>, Problem> {
    let (repo_provider, repo_info, _) = provider_for_public_request(
        state.as_ref(),
        auth.as_ref().map(|Extension(auth)| auth),
        &provider,
        &owner,
        &repo,
        params.base_url.as_deref(),
    )
    .await?;

    let target_branch = if let Some(branch) = params.branch.clone() {
        branch
    } else {
        repo_info.default_branch
    };

    let file = repo_provider
        .get_file_content(&owner, &repo, &params.path, &target_branch)
        .await
        .map_err(|e| map_error(e, &owner, &repo))?;

    let content = decode_file_content(&file.content, &file.encoding);
    let services = temps_presets::list_compose_services(&content)
        .map_err(|e| {
            problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Compose File")
                .with_detail(format!(
                    "Compose file '{}' could not be parsed: {}",
                    params.path, e
                ))
        })?
        .into_iter()
        .map(|s| PublicComposeServicePreview {
            name: s.name,
            image: s.image,
            depends_on: s.depends_on,
            environment_variables: s.environment_variables,
            looks_like_database: s.looks_like_database,
            detected_service_type: s.detected_service_type,
            ports: s.ports,
        })
        .collect();

    Ok(Json(PublicComposeServicesResponse {
        branch: target_branch,
        path: params.path,
        services,
    }))
}

/// Render a redacted effective Compose preview for a public repository.
#[utoipa::path(
    post,
    path = "/git/public/{provider}/{owner}/{repo}/compose-file",
    params(
        ("provider" = String, Path, description = "Git provider (github or gitlab)"),
        ("owner" = String, Path, description = "Repository owner"),
        ("repo" = String, Path, description = "Repository name"),
        PublicGitLabQueryParams
    ),
    request_body = PublicComposePreviewRequest,
    responses(
        (status = 200, description = "Effective Compose preview rendered", body = PublicComposePreviewResponse),
        (status = 400, description = "Compose file or override is invalid"),
        (status = 401, description = "Authentication required for custom GitLab origins"),
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository, branch, or compose file not found")
    ),
    tag = "Public Repositories"
)]
pub async fn get_public_compose_preview(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthContext>>,
    Path((provider, owner, repo)): Path<(String, String, String)>,
    Query(params): Query<PublicGitLabQueryParams>,
    Json(request): Json<PublicComposePreviewRequest>,
) -> Result<Json<PublicComposePreviewResponse>, Problem> {
    let (repo_provider, repo_info, _) = provider_for_public_request(
        state.as_ref(),
        auth.as_ref().map(|Extension(auth)| auth),
        &provider,
        &owner,
        &repo,
        params.base_url.as_deref(),
    )
    .await?;
    let target_branch = if let Some(branch) = request.branch {
        branch
    } else {
        repo_info.default_branch
    };
    let file = repo_provider
        .get_file_content(&owner, &repo, &request.path, &target_branch)
        .await
        .map_err(|error| map_error(error, &owner, &repo))?;
    let content = decode_file_content(&file.content, &file.encoding);
    let preview = temps_presets::render_effective_compose_preview(
        &content,
        request.compose_override.as_deref(),
        &request.excluded_services,
    )
    .map_err(|error| {
        problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Compose Preview")
            .with_detail(format!(
                "Compose preview for '{}' could not be rendered: {}",
                request.path, error
            ))
    })?;

    Ok(Json(PublicComposePreviewResponse {
        branch: target_branch,
        path: request.path,
        effective_compose: preview.yaml,
        enabled_services: preview.enabled_services,
        disabled_services: preview.disabled_services,
        redacted_values: preview.redacted_values,
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
        PublicGitLabQueryParams
    ),
    responses(
        (status = 200, description = "Repository information", body = PublicRepositoryInfo),
        (status = 400, description = "Provider not supported"),
        (status = 401, description = "Authentication required for custom GitLab origins"),
        (status = 403, description = "Git provider permission required"),
        (status = 404, description = "Repository not found"),
        (status = 429, description = "API rate limit exceeded"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Public Repositories"
)]
pub async fn get_public_repository(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthContext>>,
    Path((provider, owner, repo)): Path<(String, String, String)>,
    Query(params): Query<PublicGitLabQueryParams>,
) -> Result<Json<PublicRepositoryInfo>, Problem> {
    let (_repo_provider, repo_info, _) = provider_for_public_request(
        state.as_ref(),
        auth.as_ref().map(|Extension(auth)| auth),
        &provider,
        &owner,
        &repo,
        params.base_url.as_deref(),
    )
    .await?;

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
            "/git/public/{provider}/{owner}/{repo}",
            axum::routing::get(get_public_repository),
        )
        .route(
            "/git/public/{provider}/{owner}/{repo}/branches",
            axum::routing::get(get_public_branches),
        )
        .route(
            "/git/public/{provider}/{owner}/{repo}/presets",
            axum::routing::get(detect_public_presets),
        )
        .route(
            "/git/public/{provider}/{owner}/{repo}/env-example",
            axum::routing::get(detect_public_env_example),
        )
        .route(
            "/git/public/{provider}/{owner}/{repo}/compose-file",
            axum::routing::get(get_public_compose_services).post(get_public_compose_preview),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_public_repository,
        get_public_branches,
        detect_public_presets,
        detect_public_env_example,
        get_public_compose_services,
        get_public_compose_preview
    ),
    components(
        schemas(
            PublicRepositoryInfo,
            PresetInfo,
            PublicPresetResponse,
            EnvExampleVariable,
            PublicEnvExampleResponse,
            PublicComposeServicePreview,
            ComposePortMapping,
            PublicComposeServicesResponse,
            PublicComposePreviewRequest,
            PublicComposePreviewResponse,
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
    use crate::services::git_provider::FileContent;
    use crate::services::public_repo::{
        GitHubPublicProvider, GitLabPublicProvider, PublicBranch, PublicRepoInfo,
        PublicRepoProvider,
    };

    struct DockerfilePortProvider {
        fail_reads: bool,
    }

    #[async_trait::async_trait]
    impl PublicRepoProvider for DockerfilePortProvider {
        fn provider_name(&self) -> &'static str {
            "test"
        }

        async fn get_repository(
            &self,
            _owner: &str,
            _repo: &str,
        ) -> Result<PublicRepoInfo, PublicRepoError> {
            panic!("repository metadata is not needed for this test")
        }

        async fn list_branches(
            &self,
            _owner: &str,
            _repo: &str,
        ) -> Result<Vec<PublicBranch>, PublicRepoError> {
            panic!("branch listing is not needed for this test")
        }

        async fn get_file_tree(
            &self,
            _owner: &str,
            _repo: &str,
            _reference: &str,
        ) -> Result<Vec<String>, PublicRepoError> {
            panic!("file tree lookup is not needed for this test")
        }

        async fn get_file_content(
            &self,
            _owner: &str,
            _repo: &str,
            path: &str,
            _reference: &str,
        ) -> Result<FileContent, PublicRepoError> {
            if self.fail_reads {
                return Err(PublicRepoError::ApiError(format!(
                    "simulated read failure for {path}"
                )));
            }
            let content = match path {
                "Dockerfile" => "FROM alpine\nEXPOSE 3000\n",
                "apps/api/Dockerfile" => "FROM alpine\nEXPOSE 8080\n",
                other => panic!("unexpected Dockerfile path: {other}"),
            };
            Ok(FileContent {
                path: path.to_string(),
                content: content.to_string(),
                encoding: "utf-8".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn public_preset_detection_limits_anonymous_provider_work() {
        let files = vec!["Dockerfile".to_string(), "apps/api/Dockerfile".to_string()];
        let mut presets = detect_presets_from_files(&files);

        enrich_public_dockerfile_exposed_ports(
            &DockerfilePortProvider { fail_reads: false },
            "example-owner",
            "example-repository",
            "main",
            &mut presets,
        )
        .await;

        let root = presets
            .iter()
            .find(|preset| preset.path == "./")
            .expect("root Dockerfile preset should be detected");
        let api = presets
            .iter()
            .find(|preset| preset.path == "apps/api")
            .expect("nested Dockerfile preset should be detected");
        assert_eq!(root.exposed_port, Some(3000));
        assert_eq!(api.exposed_port, None);
    }

    #[tokio::test]
    async fn dockerfile_read_failure_keeps_detected_preset_available() {
        let mut presets = detect_presets_from_files(&["Dockerfile".to_string()]);

        enrich_public_dockerfile_exposed_ports(
            &DockerfilePortProvider { fail_reads: true },
            "example-owner",
            "example-repository",
            "main",
            &mut presets,
        )
        .await;

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].preset, "dockerfile");
        assert_eq!(presets[0].exposed_port, None);
    }

    // =============================================================================
    // Unit Tests - Cache Key Tests
    // =============================================================================

    #[test]
    fn self_hosted_gitlab_instances_use_separate_cache_scopes() {
        let first = public_cache_scope("gitlab", Some("https://gitlab-one.example"));
        let second = public_cache_scope("gitlab", Some("https://gitlab-two.example"));

        assert_ne!(first, second);
        assert_eq!(public_cache_scope("GitLab", None), "gitlab");
    }

    #[tokio::test]
    async fn rejects_custom_origins_for_non_gitlab_providers() {
        assert!(
            validated_gitlab_instance("github", Some("https://gitlab.example.com"))
                .await
                .is_err()
        );
    }

    #[test]
    fn custom_gitlab_origins_require_authentication() {
        let error = authorize_custom_gitlab_origin(None, Some("https://source.example.com"))
            .expect_err("custom origins must not create unauthenticated egress");
        assert_eq!(error.status_code, StatusCode::UNAUTHORIZED);

        let deployment_token = AuthContext::new_deployment_token(
            1,
            None,
            None,
            1,
            "test-token".to_string(),
            Vec::new(),
        );
        let error = authorize_custom_gitlab_origin(
            Some(&deployment_token),
            Some("https://source.example.com"),
        )
        .expect_err("a principal without repository-read permission must be denied");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert!(authorize_custom_gitlab_origin(None, None).is_ok());
    }

    #[test]
    fn fresh_public_preset_detection_requires_repository_read_permission() {
        assert!(authorize_public_preset_refresh(None, false).is_ok());
        let error = authorize_public_preset_refresh(None, true)
            .expect_err("anonymous callers must not bypass the shared preset cache");
        assert_eq!(error.status_code, StatusCode::UNAUTHORIZED);

        let deployment_token = AuthContext::new_deployment_token(
            1,
            None,
            None,
            1,
            "test-token".to_string(),
            Vec::new(),
        );
        let error = authorize_public_preset_refresh(Some(&deployment_token), true)
            .expect_err("unrelated authenticated principals must not bypass the cache");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_private_and_non_origin_gitlab_urls() {
        assert!(
            validated_gitlab_instance("gitlab", Some("https://127.0.0.1"))
                .await
                .is_err()
        );
        assert!(
            validated_gitlab_instance("gitlab", Some("https://gitlab.example.com/group"))
                .await
                .is_err()
        );
        assert!(
            validated_gitlab_instance("gitlab", Some("https://gitlab.example.com:8443"))
                .await
                .is_err()
        );
    }

    #[test]
    fn every_public_repository_operation_documents_the_gitlab_origin() {
        let document = serde_json::to_value(PublicRepositoriesApiDoc::openapi())
            .expect("serialize the public repository OpenAPI document");
        for (path, method) in [
            ("/git/public/{provider}/{owner}/{repo}", "get"),
            ("/git/public/{provider}/{owner}/{repo}/branches", "get"),
            ("/git/public/{provider}/{owner}/{repo}/compose-file", "get"),
            ("/git/public/{provider}/{owner}/{repo}/compose-file", "post"),
            ("/git/public/{provider}/{owner}/{repo}/env-example", "get"),
            ("/git/public/{provider}/{owner}/{repo}/presets", "get"),
        ] {
            let parameters = document["paths"][path][method]["parameters"]
                .as_array()
                .unwrap_or_else(|| panic!("{method} {path} must document query parameters"));
            assert!(
                parameters
                    .iter()
                    .any(|parameter| parameter["name"] == "base_url"),
                "{method} {path} must document base_url"
            );
        }
    }

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
            compose_files: None,
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
            compose_files: None,
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
                    compose_files: None,
                },
                PresetInfo {
                    path: "frontend".to_string(),
                    preset: "react".to_string(),
                    preset_label: "React".to_string(),
                    exposed_port: Some(3000),
                    icon_url: None,
                    project_type: "frontend".to_string(),
                    compose_files: None,
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
    fn test_private_repository_is_hidden_as_not_found() {
        let err = PublicRepoError::RepositoryNotPublic("owner/private".to_string());
        let problem = map_error(err, "owner", "private");
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
        assert!(!problem
            .body
            .get("detail")
            .and_then(|value| value.as_str())
            .is_some_and(|detail| detail.contains("credential") || detail.contains("token")));
    }

    #[test]
    fn test_error_mapping_rate_limit() {
        let err = PublicRepoError::RateLimitExceeded;
        let problem = map_error(err, "owner", "repo");
        assert_eq!(problem.status_code, StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_error_mapping_permission_denied() {
        let err = PublicRepoError::PermissionDenied {
            operation: "list branches for owner/repo".to_string(),
            required_permission: "Contents: read".to_string(),
        };
        let problem = map_error(err, "owner", "repo");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            problem.body.get("title").and_then(|value| value.as_str()),
            Some("Git Provider Permission Required")
        );
        assert!(problem
            .body
            .get("detail")
            .and_then(|value| value.as_str())
            .is_some_and(|detail| detail.contains("Contents: read")));
    }

    #[test]
    fn test_error_mapping_provider_not_supported() {
        let err = PublicRepoError::ProviderNotSupported("bitbucket".to_string());
        let problem = map_error(err, "owner", "repo");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn private_repository_is_rejected_before_shared_cache_use() {
        let info = crate::services::public_repo::PublicRepoInfo {
            owner: "example".to_string(),
            name: "repository".to_string(),
            full_name: "example/repository".to_string(),
            description: None,
            default_branch: "main".to_string(),
            language: None,
            stars: 0,
            forks: 0,
            is_private: true,
        };

        assert!(matches!(
            require_public_repository(info, "example", "repository"),
            Err(PublicRepoError::NotFound(message)) if message.contains("is private")
        ));
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
        let provider = GitLabPublicProvider::new();

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
        let provider = GitLabPublicProvider::new();

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
        let provider = GitLabPublicProvider::new();

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
