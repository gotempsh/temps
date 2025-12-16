//! Public repository service for accessing public repositories without authentication
//!
//! This module provides a generic interface for fetching data from public repositories
//! across different Git providers (GitHub, GitLab, etc.) without requiring authentication.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur when accessing public repositories
#[derive(Error, Debug)]
pub enum PublicRepoError {
    #[error("Repository not found: {0}")]
    NotFound(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("API error: {0}")]
    ApiError(String),

    #[error("Branch not found: {0}")]
    BranchNotFound(String),

    #[error("Provider not supported: {0}")]
    ProviderNotSupported(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Information about a public repository
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicRepoInfo {
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub default_branch: String,
    pub language: Option<String>,
    pub stars: i32,
    pub forks: i32,
}

/// Branch information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicBranch {
    pub name: String,
    pub commit_sha: String,
    pub protected: bool,
}

/// Detected preset information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedPreset {
    pub path: String,
    pub preset: String,
    pub preset_label: String,
    pub exposed_port: Option<i32>,
    pub icon_url: Option<String>,
    pub project_type: String,
}

/// Trait for public repository providers
#[async_trait]
pub trait PublicRepoProvider: Send + Sync {
    /// Get the provider name (e.g., "github", "gitlab")
    fn provider_name(&self) -> &'static str;

    /// Get repository information
    async fn get_repository(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<PublicRepoInfo, PublicRepoError>;

    /// List branches for a repository
    async fn list_branches(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<PublicBranch>, PublicRepoError>;

    /// Get the file tree for preset detection
    async fn get_file_tree(
        &self,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Vec<String>, PublicRepoError>;
}

/// GitHub public repository provider
pub struct GitHubPublicProvider {
    client: reqwest::Client,
}

impl GitHubPublicProvider {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("Temps-Engine/1.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    fn map_error(e: reqwest::Error) -> PublicRepoError {
        let error_str = e.to_string();
        if error_str.contains("404") {
            PublicRepoError::NotFound(error_str)
        } else if error_str.contains("403") || error_str.contains("rate limit") {
            PublicRepoError::RateLimitExceeded
        } else {
            PublicRepoError::ApiError(error_str)
        }
    }

    fn check_response_status(
        status: reqwest::StatusCode,
        context: &str,
    ) -> Result<(), PublicRepoError> {
        match status.as_u16() {
            200..=299 => Ok(()),
            404 => Err(PublicRepoError::NotFound(context.to_string())),
            403 | 429 => Err(PublicRepoError::RateLimitExceeded),
            _ => Err(PublicRepoError::ApiError(format!(
                "{}: HTTP {}",
                context, status
            ))),
        }
    }
}

impl Default for GitHubPublicProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PublicRepoProvider for GitHubPublicProvider {
    fn provider_name(&self) -> &'static str {
        "github"
    }

    async fn get_repository(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<PublicRepoInfo, PublicRepoError> {
        let url = format!("https://api.github.com/repos/{}/{}", owner, repo);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(Self::map_error)?;

        Self::check_response_status(response.status(), &format!("Repository {}/{}", owner, repo))?;

        #[derive(Deserialize)]
        struct GitHubRepo {
            name: String,
            full_name: String,
            description: Option<String>,
            default_branch: Option<String>,
            language: Option<serde_json::Value>,
            stargazers_count: Option<u32>,
            forks_count: Option<u32>,
            owner: Option<GitHubOwner>,
        }

        #[derive(Deserialize)]
        struct GitHubOwner {
            login: String,
        }

        let repo_data: GitHubRepo = response
            .json()
            .await
            .map_err(|e| PublicRepoError::ApiError(format!("Failed to parse response: {}", e)))?;

        Ok(PublicRepoInfo {
            owner: repo_data
                .owner
                .map(|o| o.login)
                .unwrap_or_else(|| owner.to_string()),
            name: repo_data.name,
            full_name: repo_data.full_name,
            description: repo_data.description,
            default_branch: repo_data
                .default_branch
                .unwrap_or_else(|| "main".to_string()),
            language: repo_data
                .language
                .and_then(|v| v.as_str().map(|s| s.to_string())),
            stars: repo_data.stargazers_count.unwrap_or(0) as i32,
            forks: repo_data.forks_count.unwrap_or(0) as i32,
        })
    }

    async fn list_branches(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<PublicBranch>, PublicRepoError> {
        let url = format!("https://api.github.com/repos/{}/{}/branches", owner, repo);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(Self::map_error)?;

        Self::check_response_status(
            response.status(),
            &format!("Branches for {}/{}", owner, repo),
        )?;

        #[derive(Deserialize)]
        struct GitHubBranch {
            name: String,
            commit: GitHubCommit,
            protected: bool,
        }

        #[derive(Deserialize)]
        struct GitHubCommit {
            sha: String,
        }

        let branches: Vec<GitHubBranch> = response
            .json()
            .await
            .map_err(|e| PublicRepoError::ApiError(format!("Failed to parse branches: {}", e)))?;

        Ok(branches
            .into_iter()
            .map(|b| PublicBranch {
                name: b.name,
                commit_sha: b.commit.sha,
                protected: b.protected,
            })
            .collect())
    }

    async fn get_file_tree(
        &self,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Vec<String>, PublicRepoError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
            owner, repo, reference
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(Self::map_error)?;

        Self::check_response_status(
            response.status(),
            &format!("File tree for {}/{} at {}", owner, repo, reference),
        )?;

        #[derive(Deserialize)]
        struct TreeResponse {
            tree: Vec<TreeEntry>,
        }

        #[derive(Deserialize)]
        struct TreeEntry {
            path: String,
            #[serde(rename = "type")]
            entry_type: String,
        }

        let tree_response: TreeResponse = response
            .json()
            .await
            .map_err(|e| PublicRepoError::ApiError(format!("Failed to parse tree: {}", e)))?;

        // Filter only files (blobs)
        Ok(tree_response
            .tree
            .into_iter()
            .filter(|entry| entry.entry_type == "blob")
            .map(|entry| entry.path)
            .collect())
    }
}

/// GitLab public repository provider
pub struct GitLabPublicProvider {
    client: reqwest::Client,
    base_url: String,
}

impl GitLabPublicProvider {
    pub fn new(base_url: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("Temps-Engine/1.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url: base_url.unwrap_or_else(|| "https://gitlab.com".to_string()),
        }
    }

    fn map_error(e: reqwest::Error) -> PublicRepoError {
        let error_str = e.to_string();
        if error_str.contains("404") {
            PublicRepoError::NotFound(error_str)
        } else if error_str.contains("403")
            || error_str.contains("429")
            || error_str.contains("rate limit")
        {
            PublicRepoError::RateLimitExceeded
        } else {
            PublicRepoError::ApiError(error_str)
        }
    }

    fn check_response_status(
        status: reqwest::StatusCode,
        context: &str,
    ) -> Result<(), PublicRepoError> {
        match status.as_u16() {
            200..=299 => Ok(()),
            404 => Err(PublicRepoError::NotFound(context.to_string())),
            403 | 429 => Err(PublicRepoError::RateLimitExceeded),
            _ => Err(PublicRepoError::ApiError(format!(
                "{}: HTTP {}",
                context, status
            ))),
        }
    }

    fn encode_project_path(owner: &str, repo: &str) -> String {
        urlencoding::encode(&format!("{}/{}", owner, repo)).to_string()
    }
}

impl Default for GitLabPublicProvider {
    fn default() -> Self {
        Self::new(None)
    }
}

#[async_trait]
impl PublicRepoProvider for GitLabPublicProvider {
    fn provider_name(&self) -> &'static str {
        "gitlab"
    }

    async fn get_repository(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<PublicRepoInfo, PublicRepoError> {
        let encoded_path = Self::encode_project_path(owner, repo);
        let url = format!("{}/api/v4/projects/{}", self.base_url, encoded_path);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(Self::map_error)?;

        Self::check_response_status(response.status(), &format!("Repository {}/{}", owner, repo))?;

        #[derive(Deserialize)]
        struct GitLabProject {
            name: String,
            path_with_namespace: String,
            description: Option<String>,
            default_branch: Option<String>,
            star_count: Option<i32>,
            forks_count: Option<i32>,
            namespace: Option<GitLabNamespace>,
        }

        #[derive(Deserialize)]
        struct GitLabNamespace {
            path: String,
        }

        let project: GitLabProject = response
            .json()
            .await
            .map_err(|e| PublicRepoError::ApiError(format!("Failed to parse response: {}", e)))?;

        Ok(PublicRepoInfo {
            owner: project
                .namespace
                .map(|n| n.path)
                .unwrap_or_else(|| owner.to_string()),
            name: project.name,
            full_name: project.path_with_namespace,
            description: project.description,
            default_branch: project.default_branch.unwrap_or_else(|| "main".to_string()),
            language: None, // GitLab doesn't return primary language in basic project info
            stars: project.star_count.unwrap_or(0),
            forks: project.forks_count.unwrap_or(0),
        })
    }

    async fn list_branches(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<PublicBranch>, PublicRepoError> {
        let encoded_path = Self::encode_project_path(owner, repo);
        let url = format!(
            "{}/api/v4/projects/{}/repository/branches",
            self.base_url, encoded_path
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(Self::map_error)?;

        Self::check_response_status(
            response.status(),
            &format!("Branches for {}/{}", owner, repo),
        )?;

        #[derive(Deserialize)]
        struct GitLabBranch {
            name: String,
            commit: GitLabCommit,
            protected: bool,
        }

        #[derive(Deserialize)]
        struct GitLabCommit {
            id: String,
        }

        let branches: Vec<GitLabBranch> = response
            .json()
            .await
            .map_err(|e| PublicRepoError::ApiError(format!("Failed to parse branches: {}", e)))?;

        Ok(branches
            .into_iter()
            .map(|b| PublicBranch {
                name: b.name,
                commit_sha: b.commit.id,
                protected: b.protected,
            })
            .collect())
    }

    async fn get_file_tree(
        &self,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Vec<String>, PublicRepoError> {
        let encoded_path = Self::encode_project_path(owner, repo);
        let encoded_ref = urlencoding::encode(reference);

        // GitLab requires pagination for tree, fetch all files recursively
        let mut all_files = Vec::new();
        let mut page = 1;
        let per_page = 100;

        loop {
            let url = format!(
                "{}/api/v4/projects/{}/repository/tree?ref={}&recursive=true&per_page={}&page={}",
                self.base_url, encoded_path, encoded_ref, per_page, page
            );

            let response = self
                .client
                .get(&url)
                .send()
                .await
                .map_err(Self::map_error)?;

            Self::check_response_status(
                response.status(),
                &format!("File tree for {}/{} at {}", owner, repo, reference),
            )?;

            #[derive(Deserialize)]
            struct TreeEntry {
                path: String,
                #[serde(rename = "type")]
                entry_type: String,
            }

            let entries: Vec<TreeEntry> = response
                .json()
                .await
                .map_err(|e| PublicRepoError::ApiError(format!("Failed to parse tree: {}", e)))?;

            let count = entries.len();

            // Filter only blobs (files)
            for entry in entries {
                if entry.entry_type == "blob" {
                    all_files.push(entry.path);
                }
            }

            // If we got fewer entries than per_page, we've reached the end
            if count < per_page {
                break;
            }

            page += 1;

            // Safety limit to prevent infinite loops
            if page > 100 {
                break;
            }
        }

        Ok(all_files)
    }
}

/// Factory for creating public repo providers
pub struct PublicRepoProviderFactory;

impl PublicRepoProviderFactory {
    /// Create a provider for the given provider name
    pub fn create(provider: &str) -> Result<Box<dyn PublicRepoProvider>, PublicRepoError> {
        match provider.to_lowercase().as_str() {
            "github" => Ok(Box::new(GitHubPublicProvider::new())),
            "gitlab" => Ok(Box::new(GitLabPublicProvider::new(None))),
            _ => Err(PublicRepoError::ProviderNotSupported(provider.to_string())),
        }
    }

    /// Create a GitLab provider with a custom base URL (for self-hosted instances)
    pub fn create_gitlab_with_url(base_url: &str) -> Box<dyn PublicRepoProvider> {
        Box::new(GitLabPublicProvider::new(Some(base_url.to_string())))
    }
}

/// Detect presets from a file tree
pub fn detect_presets_from_files(files: &[String]) -> Vec<DetectedPreset> {
    let detected = temps_presets::detect_presets_from_file_tree(files);

    detected
        .into_iter()
        .map(|preset| {
            let preset_enum = preset.slug.parse::<temps_entities::preset::Preset>().ok();

            let exposed_port = preset_enum
                .as_ref()
                .and_then(|p| p.exposed_port())
                .map(|p| p as i32)
                .or(preset.exposed_port.map(|p| p as i32));

            let icon_url = preset_enum
                .as_ref()
                .and_then(|p| p.icon_url())
                .map(|s| s.to_string());

            let project_type = preset_enum
                .as_ref()
                .map(|p| p.project_type().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            DetectedPreset {
                path: preset.path,
                preset: preset.slug,
                preset_label: preset.label,
                exposed_port,
                icon_url,
                project_type,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // =============================================================================
    // Unit Tests - Provider Factory
    // =============================================================================

    #[test]
    fn test_factory_creates_github_provider() {
        let provider = PublicRepoProviderFactory::create("github");
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "github");
    }

    #[test]
    fn test_factory_creates_gitlab_provider() {
        let provider = PublicRepoProviderFactory::create("gitlab");
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "gitlab");
    }

    #[test]
    fn test_factory_case_insensitive() {
        assert!(PublicRepoProviderFactory::create("GitHub").is_ok());
        assert!(PublicRepoProviderFactory::create("GITLAB").is_ok());
        assert!(PublicRepoProviderFactory::create("GiThUb").is_ok());
    }

    #[test]
    fn test_factory_unsupported_provider() {
        let result = PublicRepoProviderFactory::create("bitbucket");
        assert!(result.is_err());
        match result {
            Err(PublicRepoError::ProviderNotSupported(name)) => {
                assert_eq!(name, "bitbucket");
            }
            _ => panic!("Expected ProviderNotSupported error"),
        }
    }

    #[test]
    fn test_gitlab_custom_url() {
        let provider =
            PublicRepoProviderFactory::create_gitlab_with_url("https://gitlab.example.com");
        assert_eq!(provider.provider_name(), "gitlab");
    }

    // =============================================================================
    // Unit Tests - Error Types
    // =============================================================================

    #[test]
    fn test_error_display() {
        let not_found = PublicRepoError::NotFound("repo not found".to_string());
        assert!(not_found.to_string().contains("not found"));

        let rate_limit = PublicRepoError::RateLimitExceeded;
        assert!(rate_limit.to_string().contains("Rate limit"));

        let api_error = PublicRepoError::ApiError("connection failed".to_string());
        assert!(api_error.to_string().contains("connection failed"));
    }

    // =============================================================================
    // Unit Tests - Data Types
    // =============================================================================

    #[test]
    fn test_public_repo_info_serialization() {
        let info = PublicRepoInfo {
            owner: "facebook".to_string(),
            name: "react".to_string(),
            full_name: "facebook/react".to_string(),
            description: Some("A JavaScript library".to_string()),
            default_branch: "main".to_string(),
            language: Some("JavaScript".to_string()),
            stars: 200000,
            forks: 40000,
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"owner\":\"facebook\""));
        assert!(json.contains("\"stars\":200000"));

        let deserialized: PublicRepoInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.owner, "facebook");
        assert_eq!(deserialized.stars, 200000);
    }

    #[test]
    fn test_public_branch_serialization() {
        let branch = PublicBranch {
            name: "main".to_string(),
            commit_sha: "abc123def456".to_string(),
            protected: true,
        };

        let json = serde_json::to_string(&branch).unwrap();
        assert!(json.contains("\"name\":\"main\""));
        assert!(json.contains("\"protected\":true"));
    }

    #[test]
    fn test_detected_preset_serialization() {
        let preset = DetectedPreset {
            path: "apps/web".to_string(),
            preset: "nextjs".to_string(),
            preset_label: "Next.js".to_string(),
            exposed_port: Some(3000),
            icon_url: Some("https://example.com/icon.svg".to_string()),
            project_type: "frontend".to_string(),
        };

        let json = serde_json::to_string(&preset).unwrap();
        assert!(json.contains("\"preset\":\"nextjs\""));
        assert!(json.contains("\"exposed_port\":3000"));
    }

    // =============================================================================
    // Integration Tests - GitHub API
    // =============================================================================

    #[tokio::test]
    async fn test_github_get_repository_real_api() {
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
    async fn test_github_list_branches_real_api() {
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
    async fn test_github_get_file_tree_real_api() {
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
    async fn test_github_nonexistent_repo() {
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
    // Integration Tests - GitLab API
    // =============================================================================

    #[tokio::test]
    async fn test_gitlab_get_repository_real_api() {
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
    async fn test_gitlab_list_branches_real_api() {
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
    async fn test_gitlab_get_file_tree_real_api() {
        let provider = GitLabPublicProvider::new(None);

        // Using a smaller repo for file tree test
        match provider
            .get_file_tree("gitlab-org", "gitlab-runner", "main")
            .await
        {
            Ok(files) => {
                assert!(!files.is_empty());
                // GitLab runner is a Go project, should have go.mod
                let has_go_mod = files.iter().any(|f| f == "go.mod" || f.ends_with("go.mod"));
                assert!(has_go_mod, "GitLab Runner should have go.mod");
            }
            Err(PublicRepoError::RateLimitExceeded) => {
                eprintln!("Skipping test due to rate limit");
            }
            Err(PublicRepoError::BranchNotFound(_)) => {
                // Try with master instead
                eprintln!("main branch not found, this is expected if the default is master");
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_gitlab_nonexistent_repo() {
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
}
