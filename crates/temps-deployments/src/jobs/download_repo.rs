//! Download Repository Job
//!
//! Downloads repository source code using git provider manager

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::url_validation::{redact_url_password, validate_git_url};
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_git::GitProviderManagerTrait;
use temps_logs::{LogLevel, LogService};

/// Process-wide debug toggle: when set, deployment temp directories under
/// `/tmp/temps-deployments` are left on disk instead of being removed, so an
/// operator can inspect a failed or successful download. This is an
/// operational debug knob (restart-to-change, not per-tenant config), not
/// business configuration -- see `TEMPS_DEPLOYMENT_KEEP_TEMP_FILES` in the
/// environment variable reference.
///
/// It disables cleanup on every path, including successful deployments, so
/// leaving it set reintroduces the disk-space leak this file exists to fix.
/// The first check emits a one-time warning so an operator who set it for a
/// debug session and forgot to unset it sees it in the server logs, not only
/// in a single deployment's log.
fn keep_deployment_temp_files() -> bool {
    static WARNED: std::sync::Once = std::sync::Once::new();
    let keep = std::env::var("TEMPS_DEPLOYMENT_KEEP_TEMP_FILES").is_ok();
    if keep {
        WARNED.call_once(|| {
            tracing::warn!(
                "TEMPS_DEPLOYMENT_KEEP_TEMP_FILES is set -- deployment temp directories under \
                 /tmp/temps-deployments will NOT be cleaned up, including for successful \
                 deployments. This must not remain set in production."
            );
        });
    }
    keep
}

/// The only directory tree `create_temp_dir()` ever hands to a
/// `TempDirGuard`. Kept as a named constant so the guard's own safety check
/// (below) and the path construction can't silently drift apart.
const DEPLOYMENT_TEMP_ROOT: &str = "/tmp/temps-deployments";

/// Returns the directory roots a `TempDirGuard` is allowed to
/// `remove_dir_all()`. Anything outside these is refused, regardless of how
/// the guard was constructed -- this is deliberately checked in `Drop`
/// itself rather than trusted at each call site, so a future caller that
/// builds a `TempDirGuard` around the wrong path (a bad join, a variable
/// mix-up, a copy-pasted call elsewhere in the codebase) can't turn into a
/// silent `rm -rf` of something that isn't a scratch temp directory.
///
/// Production only allows `DEPLOYMENT_TEMP_ROOT` itself -- `std::env::temp_dir()`
/// (`/tmp` on Linux) is NOT included there, since `/tmp/temps-deployments` is
/// already a subdirectory of it: adding it would make the check accept any
/// path under `/tmp`, silently defeating the whole point of scoping removal
/// to the deployments tree. The broader OS-temp-dir root is added only under
/// `#[cfg(test)]`, because tests intentionally build guards under
/// `std::env::temp_dir()` (which is `/tmp` on Linux CI but `$TMPDIR`, e.g.
/// `/var/folders/...`, on macOS) rather than writing into the real
/// `/tmp/temps-deployments`.
#[cfg(not(test))]
fn safe_temp_roots() -> Vec<PathBuf> {
    vec![PathBuf::from(DEPLOYMENT_TEMP_ROOT)]
}

#[cfg(test)]
fn safe_temp_roots() -> Vec<PathBuf> {
    vec![PathBuf::from(DEPLOYMENT_TEMP_ROOT), std::env::temp_dir()]
}

/// True if `path` resolves inside one of `safe_temp_roots()`. Canonicalizes
/// both sides when possible so a symlinked tmp dir (e.g. macOS's
/// `/tmp` -> `/private/tmp`) doesn't produce a false negative; falls back to
/// a plain prefix comparison if canonicalization fails (path already
/// removed, or a root that doesn't exist on this platform).
fn is_within_safe_temp_root(path: &std::path::Path) -> bool {
    let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    safe_temp_roots().iter().any(|root| {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        candidate.starts_with(&root)
    })
}

/// Removes a deployment's temp directory on drop unless `disarm()` was
/// called first. Guarantees the directory created in `create_temp_dir()` is
/// cleaned up on every error path inside `download_repository()` -- without
/// this, a failure between directory creation and the final `Ok(repo_dir)`
/// (network error, invalid archive, git clone failure, etc.) leaked the
/// directory forever, since `context.work_dir` -- the only thing the
/// existing `cleanup()` trait method looks at -- is never set until the job
/// fully succeeds.
struct TempDirGuard {
    path: PathBuf,
    keep: bool,
    armed: bool,
}

impl TempDirGuard {
    fn new(path: PathBuf, keep: bool) -> Self {
        Self {
            path,
            keep,
            armed: true,
        }
    }

    /// Call on the success path: the directory is still needed by later
    /// deployment jobs (build/deploy), so it must not be removed here.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if !self.armed || self.keep {
            return;
        }
        if !is_within_safe_temp_root(&self.path) {
            tracing::error!(
                path = %self.path.display(),
                "Refusing to remove deployment temp directory: path is outside the \
                 expected temp roots. This should never happen -- treat it as a bug in \
                 whatever constructed this TempDirGuard, not as a directory to delete."
            );
            return;
        }
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {
                tracing::warn!(
                    path = %self.path.display(),
                    "🧹 Cleaned up deployment temp directory after download error"
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::error!(
                    path = %self.path.display(),
                    error = %e,
                    "Failed to clean up deployment temp directory after download error"
                );
            }
        }
    }
}

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
        // A tag may move after it is resolved. When both the human-readable
        // tag and its verified commit are present, checkout must use the
        // immutable commit while retaining the tag as deployment metadata.
        if let Some(ref commit) = self.commit_sha {
            return commit.clone();
        }

        if let Some(ref tag) = self.tag_ref {
            return tag.clone();
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
    /// Uses deployment ID + timestamp to guarantee uniqueness across concurrent deployments
    /// and across reinstalls with reused deployment IDs
    fn create_temp_dir(&self, context: &WorkflowContext) -> Result<PathBuf, WorkflowError> {
        use std::time::SystemTime;

        let unix_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| WorkflowError::Other(format!("Failed to get unix timestamp: {}", e)))?
            .as_secs();

        let temp_dir = std::path::PathBuf::from(DEPLOYMENT_TEMP_ROOT).join(format!(
            "deployment-{}-{}",
            context.deployment_id, unix_epoch
        ));
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

        // A verified commit always requires a full clone + immutable checkout,
        // even when a tag is also retained for display and audit metadata.
        if let Some(commit_sha) = self.commit_sha.as_ref() {
            self.log(context, format!("Cloning for commit SHA: {}", commit_sha))
                .await?;

            // Clone full history (no branch filter) so we can checkout any commit
            let git_url_owned = git_url.to_string();
            let repo_dir_owned = repo_dir.to_path_buf();
            let repo = tokio::task::spawn_blocking(move || {
                temps_git::services::git_ops::clone_repo(&git_url_owned, &repo_dir_owned, None)
            })
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Git clone task failed: {}", e))
            })?
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to clone public repository: {}",
                    e
                ))
            })?;

            // Checkout the specific commit
            let commit_sha_owned = commit_sha.clone();
            tokio::task::spawn_blocking(move || {
                temps_git::services::git_ops::checkout_ref(&repo, &commit_sha_owned)
            })
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Git checkout task failed: {}", e))
            })?
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to checkout commit {}: {}",
                    commit_sha, e
                ))
            })?;

            self.log(
                context,
                format!("Successfully cloned and checked out commit: {}", commit_sha),
            )
            .await?;
        } else {
            // For tags and branches, use shallow clone with branch filter
            let branch_arg = self
                .tag_ref
                .as_ref()
                .or(self.branch_ref.as_ref())
                .cloned()
                .unwrap_or_else(|| "master".to_string());

            self.log(context, format!("Cloning with branch: {}", branch_arg))
                .await?;

            let git_url_owned = git_url.to_string();
            let repo_dir_owned = repo_dir.to_path_buf();
            let branch_arg_clone = branch_arg.clone();
            tokio::task::spawn_blocking(move || {
                temps_git::services::git_ops::clone_repo(
                    &git_url_owned,
                    &repo_dir_owned,
                    Some(&branch_arg_clone),
                )
            })
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Git clone task failed: {}", e))
            })?
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to clone public repository: {}",
                    e
                ))
            })?;

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
        let keep_temp_files = keep_deployment_temp_files();
        let mut temp_dir_guard = TempDirGuard::new(temp_dir.clone(), keep_temp_files);
        if keep_temp_files {
            self.log(
                context,
                format!(
                    "🐛 TEMPS_DEPLOYMENT_KEEP_TEMP_FILES is set — {} will not be cleaned up",
                    temp_dir.display()
                ),
            )
            .await?;
        }
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
                // Defense-in-depth SSRF guard (Fix #12).
                if let Err(e) = validate_git_url(git_url) {
                    let safe_url = redact_url_password(git_url);
                    tracing::error!(
                        git_url = %safe_url,
                        error = %e,
                        "Refusing to clone: git_url failed SSRF validation"
                    );
                    return Err(WorkflowError::JobExecutionFailed(format!(
                        "git_url '{}' is not permitted: {}",
                        safe_url, e
                    )));
                }
                self.clone_public_repository(context, git_url, &repo_dir)
                    .await?;
                temp_dir_guard.disarm();
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

        // Try download archive first (faster). Wire a progress channel so the
        // download — which can take minutes on a slow link for a large repo —
        // shows steady movement in the deployment log instead of appearing stuck.
        let archive_path = temp_dir.join("source.tar.gz");
        let (progress_tx, mut progress_rx) =
            tokio::sync::mpsc::unbounded_channel::<temps_git::ArchiveProgress>();
        let progress_logger = {
            let log_writer = context.log_writer.clone();
            let log_id = self.log_id.clone();
            let log_service = self.log_service.clone();
            tokio::spawn(async move {
                while let Some(p) = progress_rx.recv().await {
                    let mb = p.downloaded_bytes as f64 / (1024.0 * 1024.0);
                    let msg = match p.total_bytes {
                        Some(total) if total > 0 => {
                            let total_mb = total as f64 / (1024.0 * 1024.0);
                            let pct = (p.downloaded_bytes as f64 / total as f64) * 100.0;
                            format!(
                                "⬇️ Downloading archive: {mb:.1} MB / {total_mb:.1} MB ({pct:.0}%)"
                            )
                        }
                        _ => format!("⬇️ Downloading archive: {mb:.1} MB"),
                    };
                    if let (Some(ref log_id), Some(ref log_service)) = (&log_id, &log_service) {
                        let _ = log_service
                            .append_structured_log(log_id, LogLevel::Info, msg.clone())
                            .await;
                    }
                    let _ = log_writer.write_log(msg).await;
                }
            })
        };

        let download_result = self
            .git_provider_manager
            .download_archive(
                connection_id,
                &self.repo_owner,
                &self.repo_name,
                &checkout_ref,
                &archive_path,
                Some(&progress_tx),
            )
            .await;
        // Drop the sender so the drain task's recv() loop ends, then await it so
        // any final progress line is flushed before we log the outcome.
        drop(progress_tx);
        let _ = progress_logger.await;

        match download_result {
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

        temp_dir_guard.disarm();
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
        // Download repository (logs written in real-time).
        //
        // The error is written into the deploy log before it propagates.
        // Without this the job is marked failed but the log simply stops after
        // "Cloning ...", so a private repo, bad credentials, a missing branch
        // or an unreachable host are all indistinguishable to the operator —
        // and self-hosted users have no other place to look. Every failure
        // reason below already carries context; it just never reached them.
        let repo_dir = match self.download_repository(&context).await {
            Ok(dir) => dir,
            Err(e) => {
                self.log(&context, format!("ERROR: {}", e)).await?;
                return Err(e);
            }
        };

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
        let work_dir = repo_dir.parent().ok_or_else(|| {
            WorkflowError::JobExecutionFailed(format!(
                "Repository path '{}' has no parent directory",
                repo_dir.display()
            ))
        })?;
        context.work_dir = Some(work_dir.to_path_buf());

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
        if keep_deployment_temp_files() {
            return Ok(());
        }
        // Clean up temporary directory if it exists. Same safety check as
        // `TempDirGuard::drop()`: `work_dir` is only ever set to a path this
        // job created under `DEPLOYMENT_TEMP_ROOT`, but this stays defensive
        // rather than trusting that invariant holds forever.
        if let Some(ref work_dir) = context.work_dir {
            if work_dir.exists() {
                if is_within_safe_temp_root(work_dir) {
                    std::fs::remove_dir_all(work_dir).map_err(WorkflowError::IoError)?;
                } else {
                    tracing::error!(
                        path = %work_dir.display(),
                        "Refusing to clean up deployment work dir: path is outside the \
                         expected temp roots."
                    );
                }
            }
        }
        Ok(())
    }

    fn cleanup_after_workflow(&self) -> bool {
        true
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
        async fn get_connection_access_token(
            &self,
            _connection_id: i32,
        ) -> Result<(String, String), GitProviderManagerError> {
            Ok(("mock-token".to_string(), "github".to_string()))
        }

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
            _progress: Option<&temps_git::ArchiveProgressSender>,
        ) -> Result<(), GitProviderManagerError> {
            // Mock returns error to test fallback to clone
            Err(GitProviderManagerError::Other(
                "Mock: archive not implemented".to_string(),
            ))
        }

        async fn push_files_and_create_pr(
            &self,
            _connection_id: i32,
            _owner: &str,
            _repo: &str,
            _branch: &str,
            _base_branch: &str,
            _files: Vec<(String, Vec<u8>)>,
            _commit_message: &str,
            _pr_title: &str,
            _pr_body: &str,
        ) -> Result<temps_git::PullRequest, temps_git::GitProviderManagerError> {
            Err(temps_git::GitProviderManagerError::Other(
                "not implemented in test".into(),
            ))
        }
        async fn mint_scoped_repo_token(
            &self,
            _: i32,
            _: &str,
            _: &str,
            _: temps_git::ScopedTokenOp,
        ) -> Result<temps_git::ScopedTokenGrant, temps_git::GitProviderManagerError> {
            Err(temps_git::GitProviderManagerError::Other(
                "not implemented in test".into(),
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

        // The verified commit must win over the mutable tag.
        assert_eq!(job.get_checkout_ref(&context), "abc123");

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

        // Commit should also win over a branch when no tag is present.
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

        // Preserve tag metadata while checking out the immutable commit.
        let context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);
        assert_eq!(job.get_checkout_ref(&context), "def456");
    }

    #[test]
    fn test_create_temp_dir_unique_per_deployment() {
        let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);

        let job = DownloadRepoJob::new(
            "test".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            1,
            git_manager,
        );

        // Two contexts with different deployment IDs
        let ctx_a = crate::test_utils::create_test_context("wf-a".to_string(), 100, 1, 1);
        let ctx_b = crate::test_utils::create_test_context("wf-b".to_string(), 200, 1, 1);

        let dir_a = job.create_temp_dir(&ctx_a).unwrap();
        let dir_b = job.create_temp_dir(&ctx_b).unwrap();

        // Directories must be different even when created in the same second
        assert_ne!(
            dir_a, dir_b,
            "Different deployment IDs must produce different paths"
        );

        // Both should contain their deployment ID
        let dir_a_str = dir_a.to_string_lossy();
        let dir_b_str = dir_b.to_string_lossy();
        assert!(
            dir_a_str.contains("deployment-100-"),
            "Path should contain deployment ID: {}",
            dir_a_str
        );
        assert!(
            dir_b_str.contains("deployment-200-"),
            "Path should contain deployment ID: {}",
            dir_b_str
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn test_temp_dir_guard_removes_directory_on_drop_by_default() {
        let dir = std::env::temp_dir().join("temps-guard-test-drop");
        std::fs::create_dir_all(&dir).unwrap();

        {
            let _guard = TempDirGuard::new(dir.clone(), false);
            // Guard stays armed and keep=false: dropping without calling
            // `disarm()` must remove the directory, exactly what should
            // happen when download_repository() bails out via `?` partway
            // through.
        }

        assert!(
            !dir.exists(),
            "TempDirGuard must remove the directory on drop when not disarmed"
        );
    }

    #[test]
    fn test_temp_dir_guard_refuses_to_remove_path_outside_safe_temp_roots() {
        // A directory that is neither under `/tmp/temps-deployments` nor
        // under `std::env::temp_dir()` -- e.g. a call site that built the
        // guard around the wrong path. This must never be deleted, no
        // matter how the guard was constructed.
        let dir = std::env::current_dir()
            .unwrap()
            .join("temps-guard-unsafe-test-dir");
        std::fs::create_dir_all(&dir).unwrap();

        {
            let _guard = TempDirGuard::new(dir.clone(), false);
        }

        assert!(
            dir.exists(),
            "TempDirGuard must refuse to remove a directory outside the safe temp roots"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_temp_dir_guard_disarm_keeps_directory() {
        let dir = std::env::temp_dir().join("temps-guard-test-disarm");
        std::fs::create_dir_all(&dir).unwrap();

        {
            let mut guard = TempDirGuard::new(dir.clone(), false);
            guard.disarm();
        }

        assert!(
            dir.exists(),
            "A disarmed guard must not remove the directory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_temp_dir_guard_keep_true_preserves_directory_even_on_error() {
        let dir = std::env::temp_dir().join("temps-guard-test-keep");
        std::fs::create_dir_all(&dir).unwrap();

        {
            let _guard = TempDirGuard::new(dir.clone(), true);
            // keep=true simulates TEMPS_DEPLOYMENT_KEEP_TEMP_FILES: even an
            // armed guard on an error path must leave the directory in place.
        }

        assert!(
            dir.exists(),
            "keep=true must preserve the directory even without disarm()"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression test for the disk-space leak: a deployment temp directory
    /// created before an error occurs during download (here, an SSRF/scheme
    /// validation failure on the git URL) must not survive the failed job.
    /// Before the `TempDirGuard`, `context.work_dir` was only ever set on
    /// the success path, so `cleanup()`/`cleanup_terminal_resources` had
    /// nothing to remove and `/tmp/temps-deployments/deployment-*` leaked
    /// forever on every failed download.
    // Serialized against `test_download_repository_keeps_temp_dir_on_early_failure_when_debug_flag_set`:
    // both tests read/write the process-wide `TEMPS_DEPLOYMENT_KEEP_TEMP_FILES`
    // env var, and cargo's default parallel test execution can interleave them
    // -- this test's `create_temp_dir()` call can observe the other test's `set_var("1")`
    // mid-flight, making its guard think cleanup should be skipped and leaking
    // a directory that then fails the assertion below.
    #[tokio::test]
    #[serial_test::serial(deployment_temp_dir_env_var)]
    async fn test_download_repository_cleans_up_temp_dir_on_early_failure() {
        let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);

        // A distinctive deployment ID keeps this test's glob isolated from
        // any other directories that might exist under /tmp/temps-deployments.
        let deployment_id = 918_273_645;

        // Pre-test cleanup: remove any dirs left by a previous run of this
        // test that was killed before its `TempDirGuard::drop` could run
        // (e.g. `cargo test` interrupted mid-suite) — a stale dir would
        // otherwise fail the assertion below spuriously. This test DOES
        // create a temp dir itself (`create_temp_dir()` runs before the
        // `validate_git_url` check that fails it); the `#[serial]` attribute
        // above is what actually prevents cross-test contamination via the
        // shared `TEMPS_DEPLOYMENT_KEEP_TEMP_FILES` env var.
        if let Ok(entries) = std::fs::read_dir("/tmp/temps-deployments") {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("deployment-{deployment_id}-"))
                {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }

        let job = DownloadRepoJob::new_public(
            "test".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            // http (not https) fails validate_git_url before any temp files
            // are written into the repo dir, isolating the guard's behavior.
            "http://example.com/owner/repo.git".to_string(),
            git_manager,
        );

        let context =
            crate::test_utils::create_test_context("wf-leak-test".to_string(), deployment_id, 1, 1);

        let result = job.download_repository(&context).await;
        assert!(result.is_err(), "invalid scheme must fail validation");

        let leaked: Vec<_> = std::fs::read_dir("/tmp/temps-deployments")
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("deployment-{}-", deployment_id))
            })
            .collect();

        assert!(
            leaked.is_empty(),
            "temp dir for deployment {} must be cleaned up after a download failure, found: {:?}",
            deployment_id,
            leaked
        );
    }

    // See the matching `#[serial]` note on
    // `test_download_repository_cleans_up_temp_dir_on_early_failure` above --
    // both tests mutate the process-wide `TEMPS_DEPLOYMENT_KEEP_TEMP_FILES`
    // env var and must not run concurrently with each other.
    #[tokio::test]
    #[serial_test::serial(deployment_temp_dir_env_var)]
    async fn test_download_repository_keeps_temp_dir_on_early_failure_when_debug_flag_set() {
        let git_manager: Arc<dyn GitProviderManagerTrait> = Arc::new(MockGitProviderManager);

        let deployment_id = 918_273_646;
        let job = DownloadRepoJob::new_public(
            "test".to_string(),
            "owner".to_string(),
            "repo".to_string(),
            "http://example.com/owner/repo.git".to_string(),
            git_manager,
        );

        let context = crate::test_utils::create_test_context(
            "wf-leak-test-2".to_string(),
            deployment_id,
            1,
            1,
        );

        // SAFETY: test-only; no other test in this process reads or asserts
        // on the absence of this var, and it is always set to the same value.
        unsafe {
            std::env::set_var("TEMPS_DEPLOYMENT_KEEP_TEMP_FILES", "1");
        }
        let result = job.download_repository(&context).await;
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("TEMPS_DEPLOYMENT_KEEP_TEMP_FILES");
        }
        assert!(result.is_err(), "invalid scheme must fail validation");

        let kept: Vec<_> = std::fs::read_dir("/tmp/temps-deployments")
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("deployment-{}-", deployment_id))
            })
            .collect();

        assert!(
            !kept.is_empty(),
            "temp dir for deployment {} must be preserved when TEMPS_DEPLOYMENT_KEEP_TEMP_FILES is set",
            deployment_id
        );
        for entry in kept {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}
