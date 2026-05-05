//! Git Provider Manager Trait
//!
//! Trait for managing git provider connections and operations.
//! This allows for dependency injection and mocking in tests.

use async_trait::async_trait;
use std::path::Path;

pub use super::git_provider::PullRequest;
pub use super::git_provider::{ScopedTokenGrant, ScopedTokenOp};

/// Error type for GitProviderManager operations
#[derive(Debug, thiserror::Error)]
pub enum GitProviderManagerError {
    #[error("Database error: {0}")]
    Database(String),
    #[error("Connection not found: {0}")]
    ConnectionNotFound(i32),
    #[error("Provider not found: {0}")]
    ProviderNotFound(i32),
    #[error("Failed to decrypt token: {0}")]
    DecryptionError(String),
    #[error("Invalid provider type: {0}")]
    InvalidProviderType(String),
    #[error("Clone error: {0}")]
    CloneError(String),
    #[error("Directory not empty: {0}")]
    DirectoryNotEmpty(String),
    #[error(
        "Provider does not support scoped per-op tokens for connection {connection_id}: {reason}"
    )]
    ScopedTokensUnsupported { connection_id: i32, reason: String },
    #[error("Other error: {0}")]
    Other(String),
}

impl From<sea_orm::DbErr> for GitProviderManagerError {
    fn from(err: sea_orm::DbErr) -> Self {
        GitProviderManagerError::Database(err.to_string())
    }
}

/// Repository information
#[derive(Debug, Clone)]
pub struct RepositoryInfo {
    pub clone_url: String,
    pub default_branch: String,
    pub owner: String,
    pub name: String,
}

/// Trait for managing git provider connections and operations
#[allow(clippy::too_many_arguments)]
#[async_trait]
pub trait GitProviderManagerTrait: Send + Sync {
    /// Clone a repository into a directory (directory must be empty)
    ///
    /// # Arguments
    /// * `connection_id` - Git provider connection ID
    /// * `repo_owner` - Repository owner/organization
    /// * `repo_name` - Repository name
    /// * `target_dir` - Target directory (must be empty)
    /// * `branch_or_ref` - Optional branch, tag, or commit SHA to checkout
    ///
    /// # Returns
    /// * `Ok(())` if clone succeeds
    /// * `Err(GitProviderManagerError::DirectoryNotEmpty)` if target directory is not empty
    /// * `Err(GitProviderManagerError::CloneError)` if clone fails
    async fn clone_repository(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
        target_dir: &Path,
        branch_or_ref: Option<&str>,
    ) -> Result<(), GitProviderManagerError>;

    /// Get repository information
    async fn get_repository_info(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
    ) -> Result<RepositoryInfo, GitProviderManagerError>;

    /// Download repository archive (tarball/zipball)
    async fn download_archive(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
        branch_or_ref: &str,
        archive_path: &Path,
    ) -> Result<(), GitProviderManagerError>;

    /// Push files to a new branch and create a pull request
    ///
    /// This method combines `push_files_to_repository` and `create_pull_request` into a
    /// single operation. It pushes the given files onto the specified branch and then
    /// opens a pull request (or merge request on GitLab) targeting `base_branch`.
    ///
    /// # Arguments
    /// * `connection_id` - Git provider connection ID
    /// * `owner` - Repository owner/organization
    /// * `repo` - Repository name
    /// * `branch` - Source branch to push files to (will be created if it doesn't exist)
    /// * `base_branch` - Target branch for the pull request
    /// * `files` - List of files to commit (path, content pairs)
    /// * `commit_message` - Commit message for the pushed files
    /// * `pr_title` - Pull request title
    /// * `pr_body` - Pull request description body
    ///
    /// # Returns
    /// * `Ok(PullRequest)` - The created pull request
    /// * `Err(GitProviderManagerError)` - If the push or PR creation fails
    /// Get the (refreshed) access token for a git provider connection,
    /// along with the provider type (e.g. "github" or "gitlab"). Used by
    /// callers that need to inject credentials into a sandbox / shell so
    /// that tools like `gh` and `glab` can authenticate.
    async fn get_connection_access_token(
        &self,
        connection_id: i32,
    ) -> Result<(String, String), GitProviderManagerError>;

    async fn push_files_and_create_pr(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: &str,
        base_branch: &str,
        files: Vec<(String, Vec<u8>)>,
        commit_message: &str,
        pr_title: &str,
        pr_body: &str,
    ) -> Result<PullRequest, GitProviderManagerError>;

    /// Mint a per-operation, single-repo, narrow-permission credential.
    ///
    /// Returns a [`ScopedTokenGrant`] suitable for direct use as
    /// `username:password` in a `git clone`/`git push` URL. The credential
    /// daemon inside a workspace container calls this for every git
    /// operation; tokens are not reused across operations and live ≤1 hour.
    ///
    /// Connections backed by PAT or OAuth (no scope-down API) return
    /// [`GitProviderManagerError::ScopedTokensUnsupported`] — the daemon
    /// must refuse the request rather than fall back to a long-lived
    /// token.
    async fn mint_scoped_repo_token(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        operation: ScopedTokenOp,
    ) -> Result<ScopedTokenGrant, GitProviderManagerError>;
}
