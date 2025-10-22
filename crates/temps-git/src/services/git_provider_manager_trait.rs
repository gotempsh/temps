//! Git Provider Manager Trait
//!
//! Trait for managing git provider connections and operations.
//! This allows for dependency injection and mocking in tests.

use async_trait::async_trait;
use std::path::Path;

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
}
