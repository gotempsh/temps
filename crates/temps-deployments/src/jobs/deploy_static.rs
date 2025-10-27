//! Deploy Static Files Job
//!
//! Deploys static files (Vite, React, etc.) to the filesystem instead of containers

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::static_deployer::{StaticDeployRequest, StaticDeployer};
use temps_deployer::ImageBuilder;
use temps_logs::{LogLevel, LogService};

/// Typed output from DeployStaticJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticDeploymentOutput {
    /// Full path to the static files directory
    pub static_dir_location: String,
    /// Number of files deployed
    pub file_count: u32,
    /// Total size in bytes
    pub total_size_bytes: u64,
}

/// Job for deploying static files to the filesystem
/// Note: This job depends on BuildImageJob, which in turn depends on DownloadRepoJob
pub struct DeployStaticJob {
    job_id: String,
    build_job_id: String,
    /// Static output directory name inside container (e.g., "/app/dist", "/app/build")
    /// This is the path inside the Docker container where the build process outputs files
    static_output_dir: String,
    /// Project slug for organizing files
    project_slug: String,
    /// Environment slug for organizing files
    environment_slug: String,
    /// Deployment slug for organizing files
    deployment_slug: String,
    /// Static deployer
    static_deployer: Arc<dyn StaticDeployer>,
    /// Image builder (for extracting files from container)
    image_builder: Arc<dyn ImageBuilder>,
    /// Optional log service
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for DeployStaticJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeployStaticJob")
            .field("job_id", &self.job_id)
            .field("build_job_id", &self.build_job_id)
            .field("static_output_dir", &self.static_output_dir)
            .field("project_slug", &self.project_slug)
            .field("environment_slug", &self.environment_slug)
            .field("deployment_slug", &self.deployment_slug)
            .field("static_deployer", &"<StaticDeployer>")
            .field("image_builder", &"<ImageBuilder>")
            .finish()
    }
}

impl DeployStaticJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        job_id: String,
        build_job_id: String,
        static_output_dir: String,
        project_slug: String,
        environment_slug: String,
        deployment_slug: String,
        static_deployer: Arc<dyn StaticDeployer>,
        image_builder: Arc<dyn ImageBuilder>,
    ) -> Self {
        Self {
            job_id,
            build_job_id,
            static_output_dir,
            project_slug,
            environment_slug,
            deployment_slug,
            static_deployer,
            image_builder,
            log_id: None,
            log_service: None,
        }
    }

    pub fn with_log_id(mut self, log_id: String) -> Self {
        self.log_id = Some(log_id);
        self
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>) -> Self {
        self.log_service = Some(log_service);
        self
    }

    /// Write log message to job-specific log file and context
    async fn log(&self, context: &WorkflowContext, message: String) -> Result<(), WorkflowError> {
        let level = Self::detect_log_level(&message);

        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_structured_log(log_id, level, message.clone())
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }

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

    /// Log an error and return a WorkflowError
    /// This ensures all errors are logged before being returned
    async fn log_and_fail(&self, context: &WorkflowContext, message: String) -> WorkflowError {
        // Log the error (ignore logging errors since we're already failing)
        let _ = self.log(context, message.clone()).await;
        WorkflowError::JobExecutionFailed(message)
    }
}

#[async_trait]
impl WorkflowTask for DeployStaticJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Deploy Static Files"
    }

    fn description(&self) -> &str {
        "Deploys static files to the filesystem for direct serving by the proxy"
    }

    fn depends_on(&self) -> Vec<String> {
        vec![self.build_job_id.clone()]
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        // Get image tag from BuildImageJob output
        let image_tag: String = context
            .get_output(&self.build_job_id, "image_tag")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("image_tag output not found".to_string())
            })?;

        self.log(
            &context,
            format!(
                "🚀 Starting static deployment for project: {}",
                self.project_slug
            ),
        )
        .await?;

        self.log(
            &context,
            format!("🐳 Extracting static files from image: {}", image_tag),
        )
        .await?;

        // Create temporary directory to extract files
        let temp_dir = std::env::temp_dir().join(format!("temps-static-{}", uuid::Uuid::new_v4()));
        if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
            return Err(self
                .log_and_fail(
                    &context,
                    format!("❌ Failed to create temp directory: {}", e),
                )
                .await);
        }

        self.log(
            &context,
            format!(
                "📂 Extracting from {} to {}",
                self.static_output_dir,
                temp_dir.display()
            ),
        )
        .await?;

        // Extract static files from the container image
        if let Err(e) = self
            .image_builder
            .extract_from_image(&image_tag, &self.static_output_dir, &temp_dir)
            .await
        {
            return Err(self
                .log_and_fail(
                    &context,
                    format!(
                        "❌ Failed to extract files from container path '{}': {}",
                        self.static_output_dir, e
                    ),
                )
                .await);
        }

        self.log(&context, "✅ Files extracted successfully".to_string())
            .await?;

        // Create deploy request
        let request = StaticDeployRequest {
            source_dir: temp_dir.clone(),
            project_slug: self.project_slug.clone(),
            environment_slug: self.environment_slug.clone(),
            deployment_slug: self.deployment_slug.clone(),
        };

        // Deploy using StaticDeployer
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

        // Clean up temporary directory
        if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
            self.log(
                &context,
                format!("⚠️  Warning: Failed to clean up temp directory: {}", e),
            )
            .await?;
        }

        // Set outputs
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
        // Verify build job output exists (check for image_tag)
        let _image_tag: String = context
            .get_output(&self.build_job_id, "image_tag")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed(format!(
                    "image_tag output not found from job '{}'",
                    self.build_job_id
                ))
            })?;

        // Verify required fields
        if self.static_output_dir.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "static_output_dir cannot be empty".to_string(),
            ));
        }

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
        // Cleanup handled by StaticDeployer if needed
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::fs as std_fs;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use temps_deployer::static_deployer::FilesystemStaticDeployer;
    use temps_deployer::{BuildResult, BuilderError, ImageBuilder};

    // Mock ImageBuilder for tests
    struct MockImageBuilder {
        extract_dir: PathBuf,
    }

    #[async_trait]
    impl ImageBuilder for MockImageBuilder {
        async fn build_image(
            &self,
            _request: temps_deployer::BuildRequest,
        ) -> Result<BuildResult, BuilderError> {
            unimplemented!("Not needed for static deployment tests")
        }

        async fn import_image(
            &self,
            _image_path: PathBuf,
            _tag: &str,
        ) -> Result<String, BuilderError> {
            unimplemented!("Not needed for static deployment tests")
        }

        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            destination_path: &std::path::Path,
        ) -> Result<(), BuilderError> {
            // Copy files from extract_dir to destination_path to simulate extraction
            std::fs::create_dir_all(destination_path)
                .map_err(|e| BuilderError::Other(format!("Failed to create destination: {}", e)))?;

            copy_dir_recursive(&self.extract_dir, destination_path)
                .map_err(|e| BuilderError::Other(format!("Failed to copy files: {}", e)))?;

            Ok(())
        }

        async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
            unimplemented!("Not needed for static deployment tests")
        }

        async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
            unimplemented!("Not needed for static deployment tests")
        }

        async fn build_image_with_callback(
            &self,
            request: temps_deployer::BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            self.build_image(request.request).await
        }
    }

    fn copy_dir_recursive(src: &PathBuf, dst: &std::path::Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if file_type.is_dir() {
                std::fs::create_dir_all(&dst_path)?;
                copy_dir_recursive(&src_path, &dst_path)?;
            } else {
                std::fs::copy(&src_path, &dst_path)?;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_deploy_static_job_with_deployer() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let built_files_dir = temp_dir.path().join("built_files");

        // Create test structure (simulating files built in container)
        std_fs::create_dir_all(&built_files_dir).unwrap();
        std_fs::create_dir_all(built_files_dir.join("assets")).unwrap();

        // Create test files (simulating built static files)
        let mut file1 = std_fs::File::create(built_files_dir.join("index.html")).unwrap();
        file1.write_all(b"<html>Test</html>").unwrap();
        drop(file1);

        let mut file2 = std_fs::File::create(built_files_dir.join("assets/app.js")).unwrap();
        file2.write_all(b"console.log('test');").unwrap();
        drop(file2);

        // Create deployer and mock image builder
        let deployer = Arc::new(FilesystemStaticDeployer::new(base_dir.clone()));
        let image_builder = Arc::new(MockImageBuilder {
            extract_dir: built_files_dir,
        });

        let job = DeployStaticJob::new(
            "deploy_static".to_string(),
            "build_image".to_string(),
            "/app/dist".to_string(),
            "my-project".to_string(),
            "production".to_string(),
            "deploy-123".to_string(),
            deployer,
            image_builder,
        );

        // Create context with build output
        let mut context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);
        context
            .set_output("build_image", "image_tag", "myapp:latest")
            .unwrap();
        context
            .set_output("build_image", "image_id", "sha256:abc123")
            .unwrap();
        context
            .set_output("build_image", "size_bytes", 1024u64)
            .unwrap();
        context
            .set_output("build_image", "build_context", "/tmp/test")
            .unwrap();

        // Execute job
        let result = job.execute(context).await;
        assert!(result.is_ok(), "Job execution failed: {:?}", result.err());

        let context = result.unwrap().context;
        let static_dir: String = context
            .get_output("deploy_static", "static_dir_location")
            .unwrap()
            .unwrap();
        let file_count: u32 = context
            .get_output("deploy_static", "file_count")
            .unwrap()
            .unwrap();

        assert_eq!(file_count, 2);
        assert!(static_dir.contains("my-project"));
        assert!(static_dir.contains("production"));
        assert!(static_dir.contains("deploy-123"));

        // The storage_path returned is relative to base_dir (security feature)
        // Need to reconstruct full path for verification
        let full_storage_path = base_dir.join(&static_dir);
        assert!(
            full_storage_path.exists(),
            "Storage path does not exist: {:?}",
            full_storage_path
        );
        assert!(
            full_storage_path.join("index.html").exists(),
            "index.html not found in {:?}",
            full_storage_path
        );
        assert!(
            full_storage_path.join("assets/app.js").exists(),
            "assets/app.js not found in {:?}",
            full_storage_path
        );
    }

    #[tokio::test]
    async fn test_deploy_static_job_validation() {
        let temp_dir = TempDir::new().unwrap();
        let base_dir = temp_dir.path().join("static");
        let deployer = Arc::new(FilesystemStaticDeployer::new(base_dir));
        let image_builder = Arc::new(MockImageBuilder {
            extract_dir: temp_dir.path().to_path_buf(),
        });

        let job = DeployStaticJob::new(
            "deploy_static".to_string(),
            "build_image".to_string(),
            "".to_string(), // Empty - should fail validation
            "my-project".to_string(),
            "production".to_string(),
            "deploy-123".to_string(),
            deployer,
            image_builder,
        );

        // Create context with build output
        let mut context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);
        context
            .set_output("build_image", "image_tag", "myapp:latest")
            .unwrap();
        context
            .set_output("build_image", "image_id", "sha256:abc123")
            .unwrap();
        context
            .set_output("build_image", "size_bytes", 1024u64)
            .unwrap();
        context
            .set_output("build_image", "build_context", "/tmp/test")
            .unwrap();

        let result = job.validate_prerequisites(&context).await;

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("static_output_dir"),
            "Expected error about static_output_dir, got: {}",
            error_msg
        );
    }
}
