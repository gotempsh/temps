//! Download Repository Job
//!
//! Downloads repository source code using git provider manager

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_git::GitProviderManagerTrait;
use temps_logs::{LogLevel, LogService};

/// Job for downloading repository source code
pub struct DownloadRepoJob {
    job_id: String,
    repo_owner: String,
    repo_name: String,
    /// Git provider connection ID (optional - not needed for public repos)
    git_provider_connection_id: Option<i32>,
    /// Direct git URL for public repos or custom git servers
    git_url: Option<String>,
    /// Whether this is a public repository (no authentication needed)
    is_public_repo: bool,
    branch_ref: Option<String>,
    tag_ref: Option<String>,
    commit_sha: Option<String>,
    project_directory: Option<String>,
    git_provider_manager: Arc<dyn GitProviderManagerTrait>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

// Manual Debug implementation since trait objects don't auto-derive Debug
impl std::fmt::Debug for DownloadRepoJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadRepoJob")
            .field("job_id", &self.job_id)
            .field("repo_owner", &self.repo_owner)
            .field("repo_name", &self.repo_name)
            .field(
                "git_provider_connection_id",
                &self.git_provider_connection_id,
            )
            .field("git_url", &self.git_url)
            .field("is_public_repo", &self.is_public_repo)
            .field("branch_ref", &self.branch_ref)
            .field("tag_ref", &self.tag_ref)
            .field("commit_sha", &self.commit_sha)
            .field("project_directory", &self.project_directory)
            .finish()
    }
}

impl DownloadRepoJob {
    /// Create a new download job for a private repository (with git provider connection)
    pub fn new(
        job_id: String,
        repo_owner: String,
        repo_name: String,
        git_provider_connection_id: i32,
        git_provider_manager: Arc<dyn GitProviderManagerTrait>,
    ) -> Self {
        Self {
            job_id,
            repo_owner,
            repo_name,
            git_provider_connection_id: Some(git_provider_connection_id),
            git_url: None,
            is_public_repo: false,
            branch_ref: None,
            tag_ref: None,
            commit_sha: None,
            project_directory: None,
            git_provider_manager,
            log_id: None,
            log_service: None,
        }
    }

    /// Create a new download job for a public repository (no authentication needed)
    pub fn new_public(
        job_id: String,
        repo_owner: String,
        repo_name: String,
        git_url: String,
        git_provider_manager: Arc<dyn GitProviderManagerTrait>,
    ) -> Self {
        Self {
            job_id,
            repo_owner,
            repo_name,
            git_provider_connection_id: None,
            git_url: Some(git_url),
            is_public_repo: true,
            branch_ref: None,
            tag_ref: None,
            commit_sha: None,
            project_directory: None,
            git_provider_manager,
            log_id: None,
            log_service: None,
        }
    }

    /// Builder methods for optional fields
    pub fn with_branch_ref(mut self, branch_ref: String) -> Self {
        self.branch_ref = Some(branch_ref);
        self
    }

    pub fn with_tag_ref(mut self, tag_ref: String) -> Self {
        self.tag_ref = Some(tag_ref);
        self
    }

    pub fn with_commit_sha(mut self, commit_sha: String) -> Self {
        self.commit_sha = Some(commit_sha);
        self
    }

    pub fn with_project_directory(mut self, project_directory: String) -> Self {
        self.project_directory = Some(project_directory);
        self
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    /// Write log message to both job-specific log file and context log writer
    async fn log(&self, context: &WorkflowContext, message: String) -> Result<(), WorkflowError> {
        // Detect log level from message content/emojis
        let level = Self::detect_log_level(&message);

        // Write structured log to job-specific log file
        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        // Also write to context log writer (for real-time streaming and test capture)
        context.log(&message).await?;
        Ok(())
    }

    /// Detect log level from message content
    fn detect_log_level(message: &str) -> LogLevel {
        if message.contains("✅") || message.contains("Complete") || message.contains("success") {
            LogLevel::Success
        } else if message.contains("❌")
            || message.contains("Failed")
            || message.contains("Error")
            || message.contains("error")
        {
            LogLevel::Error
        } else if message.contains("⏳")
            || message.contains("Waiting")
            || message.contains("warning")
        {
            LogLevel::Warning
        } else {
            LogLevel::Info
        }
    }

    /// Get the branch/ref to checkout based on priority
    fn get_checkout_ref(&self, context: &WorkflowContext) -> String {
        // Priority: tag_ref > commit_sha > branch_ref > context branch > "main"
        if let Some(ref tag) = self.tag_ref {
            return tag.clone();
        }

        if let Some(ref commit) = self.commit_sha {
            return commit.clone();
        }

        if let Some(ref branch) = self.branch_ref {
            return branch.clone();
        }

        // Try to get from context
        if let Ok(Some(branch)) = context.get_var::<String>("branch_ref") {
            return branch;
        }

        "master".to_string()
    }

    /// Create temporary directory for repository
    /// Uses unix epoch timestamp to avoid conflicts when reinstalling temps with reused deployment IDs
    fn create_temp_dir(&self, _context: &WorkflowContext) -> Result<PathBuf, WorkflowError> {
        use std::time::SystemTime;

        let unix_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| WorkflowError::Other(format!("Failed to get unix timestamp: {}", e)))?
            .as_secs();

        let temp_dir = std::path::PathBuf::from("/tmp/temps-deployments")
            .join(format!("deployment-{}", unix_epoch));
        std::fs::create_dir_all(&temp_dir).map_err(WorkflowError::IoError)?;
        Ok(temp_dir)
    }

    /// Clone a public repository using direct git clone (no authentication)
    async fn clone_public_repository(
        &self,
        context: &WorkflowContext,
        git_url: &str,
        repo_dir: &std::path::Path,
    ) -> Result<(), WorkflowError> {
        self.log(
            context,
            format!("Cloning public repository from: {}", git_url),
        )
        .await?;

        // Determine clone strategy based on what ref type we have
        // commit_sha requires full clone + checkout, branches/tags can use shallow clone with --branch
        let needs_full_clone = self.commit_sha.is_some() && self.tag_ref.is_none();

        if needs_full_clone {
            let commit_sha = self.commit_sha.as_ref().unwrap();
            self.log(context, format!("Cloning for commit SHA: {}", commit_sha))
                .await?;

            // Clone full history to ensure we have the commit
            let clone_output = tokio::process::Command::new("git")
                .arg("clone")
                .arg(git_url)
                .arg(repo_dir)
                .output()
                .await
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!("Failed to run git clone: {}", e))
                })?;

            if !clone_output.status.success() {
                let stderr = String::from_utf8_lossy(&clone_output.stderr);
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Failed to clone public repository: {}",
                    stderr
                )));
            }

            // Checkout the specific commit
            let checkout_output = tokio::process::Command::new("git")
                .arg("checkout")
                .arg(commit_sha)
                .current_dir(repo_dir)
                .output()
                .await
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!("Failed to run git checkout: {}", e))
                })?;

            if !checkout_output.status.success() {
                let stderr = String::from_utf8_lossy(&checkout_output.stderr);
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Failed to checkout commit {}: {}",
                    commit_sha, stderr
                )));
            }

            self.log(
                context,
                format!("Successfully cloned and checked out commit: {}", commit_sha),
            )
            .await?;
        } else {
            // For tags and branches, use --branch with shallow clone
            // Priority: tag_ref > branch_ref > default branch
            let branch_arg = self
                .tag_ref
                .as_ref()
                .or(self.branch_ref.as_ref())
                .cloned()
                .unwrap_or_else(|| "master".to_string());

            self.log(context, format!("Cloning with --branch {}", branch_arg))
                .await?;

            let output = tokio::process::Command::new("git")
                .arg("clone")
                .arg("--depth=1")
                .arg("--branch")
                .arg(&branch_arg)
                .arg(git_url)
                .arg(repo_dir)
                .output()
                .await
                .map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!("Failed to run git clone: {}", e))
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(WorkflowError::JobExecutionFailed(format!(
                    "Failed to clone public repository: {}",
                    stderr
                )));
            }

            self.log(
                context,
                format!("Successfully cloned at ref: {}", branch_arg),
            )
            .await?;
        }

        Ok(())
    }

    /// Download repository source code with real-time logging
    async fn download_repository(
        &self,
        context: &WorkflowContext,
    ) -> Result<PathBuf, WorkflowError> {
        self.log(
            context,
            format!(
                "🔽 Starting repository download for {}/{}",
                self.repo_owner, self.repo_name
            ),
        )
        .await?;

        let checkout_ref = self.get_checkout_ref(context);
        self.log(context, format!("Checking out ref: {}", checkout_ref))
            .await?;

        // Create temp directory
        let temp_dir = self.create_temp_dir(context)?;
        let repo_dir = temp_dir.join("repository");
        std::fs::create_dir_all(&repo_dir).map_err(WorkflowError::IoError)?;

        self.log(
            context,
            format!("Created repository directory at: {}", repo_dir.display()),
        )
        .await?;

        // Handle public repos differently - use direct git clone
        if self.is_public_repo {
            if let Some(ref git_url) = self.git_url {
                self.clone_public_repository(context, git_url, &repo_dir)
                    .await?;
                return Ok(repo_dir);
            } else {
                return Err(WorkflowError::JobExecutionFailed(
                    "Public repository requires git_url to be set".to_string(),
                ));
            }
        }

        // For private repos, verify we have a connection ID
        let connection_id = self.git_provider_connection_id.ok_or_else(|| {
            WorkflowError::JobExecutionFailed(
                "Private repository requires git_provider_connection_id".to_string(),
            )
        })?;

        // Try download archive first (faster)
        let archive_path = temp_dir.join("source.tar.gz");
        match self
            .git_provider_manager
            .download_archive(
                connection_id,
                &self.repo_owner,
                &self.repo_name,
                &checkout_ref,
                &archive_path,
            )
            .await
        {
            Ok(()) => {
                self.log(
                    context,
                    "📦 Successfully downloaded repository archive".to_string(),
                )
                .await?;

                // Extract the archive
                let output = tokio::process::Command::new("tar")
                    .arg("--strip-components=1")
                    .arg("-xzf")
                    .arg(&archive_path)
                    .arg("-C")
                    .arg(&repo_dir)
                    .output()
                    .await
                    .map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to run tar command: {}",
                            e
                        ))
                    })?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(WorkflowError::JobExecutionFailed(format!(
                        "Failed to extract archive: {}",
                        stderr
                    )));
                }

                self.log(
                    context,
                    "📂 Successfully extracted repository archive".to_string(),
                )
                .await?;

                // Clean up archive
                if let Err(e) = std::fs::remove_file(&archive_path) {
                    self.log(
                        context,
                        format!("Warning: Failed to clean up archive file: {}", e),
                    )
                    .await?;
                }
            }
            Err(e) => {
                self.log(
                    context,
                    format!(
                        "📦 Archive download failed, falling back to git clone: {}",
                        e
                    ),
                )
                .await?;

                // Fall back to git clone - directory must be empty for trait method
                // Remove directory (and any contents) before cloning
                std::fs::remove_dir_all(&repo_dir).map_err(|e| {
                    WorkflowError::JobExecutionFailed(format!(
                        "Failed to remove directory for clone: {}",
                        e
                    ))
                })?;

                self.git_provider_manager
                    .clone_repository(
                        connection_id,
                        &self.repo_owner,
                        &self.repo_name,
                        &repo_dir,
                        Some(&checkout_ref),
                    )
                    .await
                    .map_err(|e| {
                        WorkflowError::JobExecutionFailed(format!(
                            "Failed to clone repository: {}",
                            e
                        ))
                    })?;

                self.log(context, "Successfully cloned repository".to_string())
                    .await?;
            }
        }

        // Validate repository was downloaded
        if !repo_dir.exists() || std::fs::read_dir(&repo_dir)?.next().is_none() {
            return Err(WorkflowError::JobExecutionFailed(
                "Repository directory is empty".to_string(),
            ));
        }

        self.log(context, "Repository validation passed".to_string())
            .await?;

        Ok(repo_dir)
    }
}

#[async_trait]
impl WorkflowTask for DownloadRepoJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Download Repository"
    }

    fn description(&self) -> &str {
        "Downloads repository source code from the configured git provider"
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        // Download repository (logs written in real-time)
        let repo_dir = self.download_repository(&context).await?;

        // Set job outputs
        context.set_output(
            &self.job_id,
            "repo_dir",
            repo_dir.to_string_lossy().to_string(),
        )?;
        context.set_output(
            &self.job_id,
            "checkout_ref",
            self.get_checkout_ref(&context),
        )?;
        context.set_output(&self.job_id, "repo_owner", &self.repo_owner)?;
        context.set_output(&self.job_id, "repo_name", &self.repo_name)?;

        // Set artifacts
        context.set_artifact(&self.job_id, "source_code", repo_dir.clone());

        // Update working directory in context
        context.work_dir = Some(repo_dir.parent().unwrap().to_path_buf());

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(
        &self,
        _context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        // Basic validation
        if self.repo_owner.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "repo_owner cannot be empty".to_string(),
            ));
        }
        if self.repo_name.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "repo_name cannot be empty".to_string(),
            ));
        }

        // For private repos, git_provider_connection_id is required
        // For public repos, git_url is required
        if !self.is_public_repo && self.git_provider_connection_id.is_none() {
            return Err(WorkflowError::JobValidationFailed(
                "git_provider_connection_id must be provided for private repositories".to_string(),
            ));
        }

        if self.is_public_repo && self.git_url.is_none() {
            return Err(WorkflowError::JobValidationFailed(
                "git_url must be provided for public repositories".to_string(),
            ));
        }

        Ok(())
    }

    async fn cleanup(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Clean up temporary directory if it exists
        if let Some(ref work_dir) = context.work_dir {
            if work_dir.exists() {
                std::fs::remove_dir_all(work_dir).map_err(WorkflowError::IoError)?;
            }
        }
        Ok(())
    }
}

/// Builder for DownloadRepoJob
pub struct DownloadRepoBuilder {
    job_id: Option<String>,
    repo_owner: Option<String>,
    repo_name: Option<String>,
    git_provider_connection_id: Option<i32>,
    git_url: Option<String>,
    is_public_repo: bool,
    branch_ref: Option<String>,
    tag_ref: Option<String>,
    commit_sha: Option<String>,
    project_directory: Option<String>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl DownloadRepoBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            repo_owner: None,
            repo_name: None,
            git_provider_connection_id: None,
            git_url: None,
            is_public_repo: false,
            branch_ref: None,
            tag_ref: None,
            commit_sha: None,
            project_directory: None,
            log_id: None,
            log_service: None,
        }
    }

    pub fn job_id(mut self, job_id: String) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn repo_owner(mut self, repo_owner: String) -> Self {
        self.repo_owner = Some(repo_owner);
        self
    }

    pub fn repo_name(mut self, repo_name: String) -> Self {
        self.repo_name = Some(repo_name);
        self
    }

    pub fn git_provider_connection_id(mut self, connection_id: i32) -> Self {
        self.git_provider_connection_id = Some(connection_id);
        self
    }

    pub fn git_url(mut self, git_url: String) -> Self {
        self.git_url = Some(git_url);
        self
    }

    pub fn is_public_repo(mut self, is_public: bool) -> Self {
        self.is_public_repo = is_public;
        self
    }

    pub fn branch_ref(mut self, branch_ref: String) -> Self {
        self.branch_ref = Some(branch_ref);
        self
    }

    pub fn tag_ref(mut self, tag_ref: String) -> Self {
        self.tag_ref = Some(tag_ref);
        self
    }

    pub fn commit_sha(mut self, commit_sha: String) -> Self {
        self.commit_sha = Some(commit_sha);
        self
    }

    pub fn project_directory(mut self, project_directory: String) -> Self {
        self.project_directory = Some(project_directory);
        self
    }

    pub fn log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    pub fn build(
        self,
        git_provider_manager: Arc<dyn GitProviderManagerTrait>,
    ) -> Result<DownloadRepoJob, WorkflowError> {
        let job_id = self.job_id.unwrap_or_else(|| "download_repo".to_string());
        let repo_owner = self.repo_owner.ok_or_else(|| {
            WorkflowError::JobValidationFailed("repo_owner is required".to_string())
        })?;
        let repo_name = self.repo_name.ok_or_else(|| {
            WorkflowError::JobValidationFailed("repo_name is required".to_string())
        })?;

        // Create job based on whether it's a public or private repo
        let mut job = if self.is_public_repo {
            // Public repo: requires git_url
            let git_url = self.git_url.ok_or_else(|| {
                WorkflowError::JobValidationFailed(
                    "git_url is required for public repositories".to_string(),
                )
            })?;
            DownloadRepoJob::new_public(
                job_id,
                repo_owner,
                repo_name,
                git_url,
                git_provider_manager,
            )
        } else {
            // Private repo: requires git_provider_connection_id
            let git_provider_connection_id = self.git_provider_connection_id.ok_or_else(|| {
                WorkflowError::JobValidationFailed(
                    "git_provider_connection_id is required for private repositories".to_string(),
                )
            })?;
            DownloadRepoJob::new(
                job_id,
                repo_owner,
                repo_name,
                git_provider_connection_id,
                git_provider_manager,
            )
        };

        if let Some(branch_ref) = self.branch_ref {
            job = job.with_branch_ref(branch_ref);
        }
        if let Some(tag_ref) = self.tag_ref {
            job = job.with_tag_ref(tag_ref);
        }
        if let Some(commit_sha) = self.commit_sha {
            job = job.with_commit_sha(commit_sha);
        }
        if let Some(project_directory) = self.project_directory {
            job = job.with_project_directory(project_directory);
        }
        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
        }

        Ok(job)
    }
}

impl Default for DownloadRepoBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use temps_git::GitProviderManagerError;

    /// Mock implementation of GitProviderManagerTrait for testing
    struct MockGitProviderManager;

    #[async_trait]
    impl GitProviderManagerTrait for MockGitProviderManager {
        async fn clone_repository(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
            _target_dir: &Path,
            _branch_or_ref: Option<&str>,
        ) -> Result<(), GitProviderManagerError> {
            // Mock implementation - just returns Ok
            Ok(())
        }

        async fn get_repository_info(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
        ) -> Result<temps_git::RepositoryInfo, GitProviderManagerError> {
            Ok(temps_git::RepositoryInfo {
                clone_url: "https://github.com/test/repo.git".to_string(),
                default_branch: "main".to_string(),
                owner: "test".to_string(),
                name: "repo".to_string(),
            })
        }

        async fn download_archive(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
            _branch_or_ref: &str,
            _archive_path: &Path,
        ) -> Result<(), GitProviderManagerError> {
            // Mock returns error to test fallback to clone
            Err(GitProviderManagerError::Other(
                "Mock: archive not implemented".to_string(),
            ))
        }
    }

    #[test]
    fn test_download_repo_builder() {
        let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);

        let job = DownloadRepoBuilder::new()
            .job_id("test_download".to_string())
            .repo_owner("test_owner".to_string())
            .repo_name("test_repo".to_string())
            .git_provider_connection_id(1)
            .branch_ref("main".to_string())
            .build(git_manager)
            .unwrap();

        assert_eq!(job.job_id(), "test_download");
        assert_eq!(job.repo_owner, "test_owner");
        assert_eq!(job.repo_name, "test_repo");
        assert_eq!(job.branch_ref, Some("main".to_string()));
    }

    #[test]
    fn test_get_checkout_ref_priority() {
        let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);

        let job = DownloadRepoJob::new(
            "test".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            1,
            git_manager.clone(),
        )
        .with_branch_ref("branch".to_string())
        .with_tag_ref("v1.0.0".to_string())
        .with_commit_sha("abc123".to_string());

        let context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);

        // Tag should have highest priority
        assert_eq!(job.get_checkout_ref(&context), "v1.0.0");

        // Test without tag
        let job_no_tag = DownloadRepoJob::new(
            "test".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            1,
            git_manager.clone(),
        )
        .with_branch_ref("branch".to_string())
        .with_commit_sha("abc123".to_string());

        // Commit should have second priority
        assert_eq!(job_no_tag.get_checkout_ref(&context), "abc123");
    }

    #[test]
    fn test_get_checkout_ref_branch_only() {
        let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);

        // Test with only branch_ref set
        let job_branch_only = DownloadRepoJob::new(
            "test".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            1,
            git_manager.clone(),
        )
        .with_branch_ref("feature-branch".to_string());

        let context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);

        // Branch should be used when no tag or commit is set
        assert_eq!(job_branch_only.get_checkout_ref(&context), "feature-branch");

        // Test with no refs set (should fall back to "master")
        let job_no_refs = DownloadRepoJob::new(
            "test".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            1,
            git_manager,
        );

        // Should fall back to "master" when nothing is set
        assert_eq!(job_no_refs.get_checkout_ref(&context), "master");
    }

    #[test]
    fn test_builder_with_tag_and_commit() {
        let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);

        let job = DownloadRepoBuilder::new()
            .job_id("test_download".to_string())
            .repo_owner("test_owner".to_string())
            .repo_name("test_repo".to_string())
            .git_provider_connection_id(1)
            .branch_ref("main".to_string())
            .tag_ref("v2.0.0".to_string())
            .commit_sha("def456".to_string())
            .build(git_manager)
            .unwrap();

        assert_eq!(job.job_id(), "test_download");
        assert_eq!(job.branch_ref, Some("main".to_string()));
        assert_eq!(job.tag_ref, Some("v2.0.0".to_string()));
        assert_eq!(job.commit_sha, Some("def456".to_string()));

        // Verify tag has highest priority
        let context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);
        assert_eq!(job.get_checkout_ref(&context), "v2.0.0");
    }
}
