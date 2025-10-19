use async_trait::async_trait;
use super::git_provider::{Commit, Branch, GitProviderTag, GitProviderError};

/// Repository-specific Git API trait
/// Provides operations for a specific repository without needing to pass owner/repo repeatedly
#[async_trait]
pub trait GitRepositoryApi: Send + Sync {
    /// Get commit information by SHA or reference
    async fn get_commit_info(&self, commit_sha: &str) -> Result<Commit, GitProviderError>;

    /// List all branches in the repository
    async fn get_branches(&self) -> Result<Vec<Branch>, GitProviderError>;

    /// Get the latest commit on a branch
    async fn get_latest_commit(&self, branch: &str) -> Result<Commit, GitProviderError>;

    /// List all tags in the repository
    async fn get_tags(&self) -> Result<Vec<GitProviderTag>, GitProviderError>;

    /// Check if a commit exists
    async fn check_commit_exists(&self, commit_sha: &str) -> Result<bool, GitProviderError>;
}

/// Implementation of GitRepositoryApi that holds connection context
pub struct GitRepositoryApiImpl {
    owner: String,
    repo: String,
    provider_service: std::sync::Arc<dyn super::git_provider::GitProviderService>,
    access_token: String,
}

impl GitRepositoryApiImpl {
    pub fn new(
        owner: String,
        repo: String,
        provider_service: std::sync::Arc<dyn super::git_provider::GitProviderService>,
        access_token: String,
    ) -> Self {
        Self {
            owner,
            repo,
            provider_service,
            access_token,
        }
    }
}

#[async_trait]
impl GitRepositoryApi for GitRepositoryApiImpl {
    async fn get_commit_info(&self, commit_sha: &str) -> Result<Commit, GitProviderError> {
        self.provider_service
            .get_commit(&self.access_token, &self.owner, &self.repo, commit_sha)
            .await
    }

    async fn get_branches(&self) -> Result<Vec<Branch>, GitProviderError> {
        self.provider_service
            .list_branches(&self.access_token, &self.owner, &self.repo)
            .await
    }

    async fn get_latest_commit(&self, branch: &str) -> Result<Commit, GitProviderError> {
        self.provider_service
            .get_latest_commit(&self.access_token, &self.owner, &self.repo, branch)
            .await
    }

    async fn get_tags(&self) -> Result<Vec<GitProviderTag>, GitProviderError> {
        self.provider_service
            .list_tags(&self.access_token, &self.owner, &self.repo)
            .await
    }

    async fn check_commit_exists(&self, commit_sha: &str) -> Result<bool, GitProviderError> {
        self.provider_service
            .check_commit_exists(&self.access_token, &self.owner, &self.repo, commit_sha)
            .await
    }
}