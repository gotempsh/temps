// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deploy Static Files From Source Job
//!
//! Deploys a static site (plain HTML/CSS/JS, no build step) straight from the
//! downloaded git checkout to the filesystem for direct serving by the proxy.
//! Unlike [`super::deploy_static::DeployStaticJob`] there is no Docker image
//! involved at all: no build, no autopack, no container ever runs. This is
//! the job the workflow planner picks when the resolved preset reports
//! `Preset::needs_container_build() == false`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use temps_core::{
    static_files::is_sensitive_static_path, JobResult, WorkflowContext, WorkflowError, WorkflowTask,
};
use temps_deployer::static_deployer::{StaticDeployRequest, StaticDeployer};
use temps_logs::{LogLevel, LogService};

use super::RepositoryOutput;

/// Typed output from DeployStaticFromSourceJob — matches
/// [`super::deploy_static::StaticDeploymentOutput`] so `mark_deployment_complete`
/// reads both job types identically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticFromSourceOutput {
    pub static_dir_location: String,
    pub file_count: u32,
    pub total_size_bytes: u64,
}

/// Deploys a static site directly from the downloaded source directory.
pub struct DeployStaticFromSourceJob {
    job_id: String,
    download_job_id: String,
    /// Project subdirectory to serve, relative to the repository root
    /// (mirrors the `directory` used for the container build context).
    directory: String,
    project_slug: String,
    environment_slug: String,
    deployment_slug: String,
    static_deployer: std::sync::Arc<dyn StaticDeployer>,
    log_id: Option<String>,
    log_service: Option<std::sync::Arc<LogService>>,
}

impl std::fmt::Debug for DeployStaticFromSourceJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeployStaticFromSourceJob")
            .field("job_id", &self.job_id)
            .field("download_job_id", &self.download_job_id)
            .field("directory", &self.directory)
            .field("project_slug", &self.project_slug)
            .field("environment_slug", &self.environment_slug)
            .field("deployment_slug", &self.deployment_slug)
            .field("static_deployer", &"<StaticDeployer>")
            .finish()
    }
}

impl DeployStaticFromSourceJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        job_id: String,
        download_job_id: String,
        directory: String,
        project_slug: String,
        environment_slug: String,
        deployment_slug: String,
        static_deployer: std::sync::Arc<dyn StaticDeployer>,
    ) -> Self {
        Self {
            job_id,
            download_job_id,
            directory,
            project_slug,
            environment_slug,
            deployment_slug,
            static_deployer,
            log_id: None,
            log_service: None,
        }
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: std::sync::Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    async fn log(&self, context: &WorkflowContext, message: String) -> Result<(), WorkflowError> {
        let level = if message.contains('❌') || message.to_ascii_lowercase().contains("failed") {
            LogLevel::Error
        } else if message.contains('✅') {
            LogLevel::Success
        } else {
            LogLevel::Info
        };

        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }

        context.log(&message).await?;
        Ok(())
    }

    async fn log_and_fail(&self, context: &WorkflowContext, message: String) -> WorkflowError {
        let _ = self.log(context, message.clone()).await;
        WorkflowError::JobExecutionFailed(message)
    }

    /// Resolve the project's configured subdirectory against the repository
    /// root, rejecting anything that could escape it. Mirrors the build
    /// context resolution in `build_image.rs`.
    fn resolve_source_dir(&self, repo_dir: &Path) -> Result<PathBuf, String> {
        let directory = Path::new(&self.directory);
        if directory.is_absolute()
            || directory.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(format!(
                "Project directory '{}' must be relative and contained by the source root",
                directory.display()
            ));
        }

        let source_dir = repo_dir.join(directory);
        let canonical_root = repo_dir
            .canonicalize()
            .map_err(|e| format!("Failed to resolve source root: {e}"))?;
        let canonical_source = source_dir.canonicalize().map_err(|e| {
            format!(
                "Failed to resolve project directory '{}': {e}",
                directory.display()
            )
        })?;
        if !canonical_source.starts_with(&canonical_root) {
            return Err(format!(
                "Project directory '{}' escapes source root '{}'",
                canonical_source.display(),
                canonical_root.display()
            ));
        }

        Ok(source_dir)
    }

    /// Copy `source` into `dest`, skipping any entry whose path fails the
    /// shared static-artifact sensitivity policy (`is_sensitive_static_path`)
    /// instead of failing the whole deployment.
    ///
    /// A raw git checkout routinely carries files like `.gitignore` or
    /// `.github/` that were never meant to be served and are never something
    /// the repository owner is asking to publish — unlike an uploaded
    /// archive or a build tool's output directory, where the same policy
    /// violation in `StaticDeployer::deploy` hard-fails the deployment on
    /// purpose (there, a stray `.env` is a real red flag worth surfacing).
    /// The policy predicate itself is untouched and still enforced a second
    /// time by `StaticDeployer::deploy` on whatever survives this filter, and
    /// a third time per-request by the proxy — this only changes the failure
    /// mode for the *first* of those three checks.
    fn copy_publishable_tree<'a>(
        source_root: &'a Path,
        source: &'a Path,
        dest: &'a Path,
        skipped: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            tokio::fs::create_dir_all(dest).await?;
            let mut entries = tokio::fs::read_dir(source).await?;
            while let Some(entry) = entries.next_entry().await? {
                let file_type = entry.file_type().await?;
                let source_path = entry.path();
                let relative = source_path
                    .strip_prefix(source_root)
                    .unwrap_or(&source_path);

                if is_sensitive_static_path(relative) {
                    skipped.push(relative.display().to_string());
                    continue;
                }

                let dest_path = dest.join(entry.file_name());
                if file_type.is_symlink() {
                    // Symlinks are rejected by the shared policy layer when
                    // present; skip rather than follow them here too.
                    skipped.push(relative.display().to_string());
                    continue;
                } else if file_type.is_dir() {
                    Self::copy_publishable_tree(source_root, &source_path, &dest_path, skipped)
                        .await?;
                } else {
                    tokio::fs::copy(&source_path, &dest_path).await?;
                }
            }
            Ok(())
        })
    }
}

#[async_trait]
impl WorkflowTask for DeployStaticFromSourceJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Deploy Static Files"
    }

    fn description(&self) -> &str {
        "Deploys static files from the repository directly to the filesystem for serving by the proxy — no build, no container"
    }

    fn depends_on(&self) -> Vec<String> {
        vec![self.download_job_id.clone()]
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        let repo_output = RepositoryOutput::from_context(&context, &self.download_job_id)?;

        self.log(
            &context,
            format!(
                "🚀 Starting static deployment for project: {} (no build step)",
                self.project_slug
            ),
        )
        .await?;

        let source_dir = self
            .resolve_source_dir(&repo_output.repo_dir)
            .map_err(|error| {
                WorkflowError::JobValidationFailed(format!(
                    "Invalid static source directory: {error}"
                ))
            })?;

        let _extraction_permit = super::acquire_archive_extraction_permit().await?;

        let temp_dir = match tempfile::Builder::new()
            .prefix("temps-static-src-")
            .tempdir()
        {
            Ok(temp_dir) => temp_dir,
            Err(error) => {
                return Err(self
                    .log_and_fail(
                        &context,
                        format!("❌ Failed to create secure temp directory: {error}"),
                    )
                    .await);
            }
        };

        self.log(
            &context,
            format!(
                "📂 Copying publishable files from {} to {}",
                source_dir.display(),
                temp_dir.path().display()
            ),
        )
        .await?;

        let mut skipped = Vec::new();
        if let Err(error) =
            Self::copy_publishable_tree(&source_dir, &source_dir, temp_dir.path(), &mut skipped)
                .await
        {
            return Err(self
                .log_and_fail(
                    &context,
                    format!(
                        "❌ Failed to copy files from '{}': {error}",
                        source_dir.display()
                    ),
                )
                .await);
        }

        if !skipped.is_empty() {
            self.log(
                &context,
                format!(
                    "⏭️  Skipped {} non-publishable path(s), e.g. {}",
                    skipped.len(),
                    skipped
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .await?;
        }

        let request = StaticDeployRequest {
            source_dir: temp_dir.path().to_path_buf(),
            project_slug: self.project_slug.clone(),
            environment_slug: self.environment_slug.clone(),
            deployment_slug: self.deployment_slug.clone(),
        };

        let result = match self.static_deployer.deploy(request).await {
            Ok(result) => result,
            Err(e) => {
                return Err(self
                    .log_and_fail(&context, format!("❌ Failed to deploy static files: {}", e))
                    .await);
            }
        };

        self.log(&context, format!("📍 Deployed to: {}", result.storage_path))
            .await?;

        context.set_output(&self.job_id, "static_dir_location", &result.storage_path)?;
        context.set_output(&self.job_id, "file_count", result.file_count)?;
        context.set_output(&self.job_id, "total_size_bytes", result.total_size_bytes)?;

        self.log(
            &context,
            format!(
                "✅ Static deployment complete: {} files deployed ({} bytes)",
                result.file_count, result.total_size_bytes
            ),
        )
        .await?;

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        RepositoryOutput::from_context(context, &self.download_job_id)?;

        if self.project_slug.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "project_slug cannot be empty".to_string(),
            ));
        }
        if self.environment_slug.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "environment_slug cannot be empty".to_string(),
            ));
        }
        if self.deployment_slug.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "deployment_slug cannot be empty".to_string(),
            ));
        }

        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use temps_deployer::static_deployer::FilesystemStaticDeployer;

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn context_with_repo_dir(repo_dir: &Path) -> WorkflowContext {
        let mut context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);
        context
            .set_output(
                "download_repo",
                "repo_dir",
                repo_dir.to_string_lossy().to_string(),
            )
            .unwrap();
        context
            .set_output("download_repo", "checkout_ref", "HEAD")
            .unwrap();
        context
            .set_output("download_repo", "repo_owner", "octocat")
            .unwrap();
        context
            .set_output("download_repo", "repo_name", "site")
            .unwrap();
        context
    }

    #[tokio::test]
    async fn deploys_a_plain_static_repo_skipping_dotfiles() {
        let repo = tempfile::tempdir().unwrap();
        write_file(&repo.path().join("index.html"), "<h1>hi</h1>");
        write_file(&repo.path().join("assets/app.js"), "console.log(1)");
        write_file(&repo.path().join(".gitignore"), "node_modules\n");
        write_file(&repo.path().join("README.md"), "# hi");

        let base_dir = tempfile::tempdir().unwrap();
        let deployer = Arc::new(FilesystemStaticDeployer::new(base_dir.path().to_path_buf()));

        let job = DeployStaticFromSourceJob::new(
            "deploy_static".to_string(),
            "download_repo".to_string(),
            ".".to_string(),
            "my-project".to_string(),
            "production".to_string(),
            "deploy-123".to_string(),
            deployer,
        );

        let context = context_with_repo_dir(repo.path());
        let result = job.execute(context).await;
        assert!(result.is_ok(), "job failed: {:?}", result.err());

        let context = result.unwrap().context;
        let static_dir: String = context
            .get_output("deploy_static", "static_dir_location")
            .unwrap()
            .unwrap();
        let file_count: u32 = context
            .get_output("deploy_static", "file_count")
            .unwrap()
            .unwrap();

        // index.html + assets/app.js + README.md, but NOT .gitignore.
        assert_eq!(
            file_count, 3,
            "dotfile should have been skipped, not counted"
        );

        let full_path = base_dir.path().join(&static_dir);
        assert!(full_path.join("index.html").exists());
        assert!(full_path.join("assets/app.js").exists());
        assert!(!full_path.join(".gitignore").exists());
        // README.md is not a dotfile and not on the sensitive list — it is
        // ordinary (if surprising) publishable content, same as it would be
        // if this repo were deployed via an uploaded archive instead.
        assert!(full_path.join("README.md").exists());
    }

    #[tokio::test]
    async fn refuses_a_directory_that_escapes_the_source_root() {
        let repo = tempfile::tempdir().unwrap();
        write_file(&repo.path().join("index.html"), "<h1>hi</h1>");

        let base_dir = tempfile::tempdir().unwrap();
        let deployer = Arc::new(FilesystemStaticDeployer::new(base_dir.path().to_path_buf()));

        let job = DeployStaticFromSourceJob::new(
            "deploy_static".to_string(),
            "download_repo".to_string(),
            "../escape".to_string(),
            "my-project".to_string(),
            "production".to_string(),
            "deploy-123".to_string(),
            deployer,
        );

        let context = context_with_repo_dir(repo.path());
        let result = job.execute(context).await;
        assert!(result.is_err(), "parent-dir escape must be rejected");
    }
}
