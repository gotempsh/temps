//! Build Image Job
//!
//! Builds container images from downloaded repository source code

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_deployer::{BuildRequest, ImageBuilder};
use temps_logs::LogService;
use temps_presets;

/// Typed output from DownloadRepoJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryOutput {
    pub repo_dir: PathBuf,
    pub checkout_ref: String,
    pub repo_owner: String,
    pub repo_name: String,
}

impl RepositoryOutput {
    /// Extract RepositoryOutput from WorkflowContext
    pub fn from_context(
        context: &WorkflowContext,
        download_job_id: &str,
    ) -> Result<Self, WorkflowError> {
        let repo_dir_str: String = context
            .get_output(download_job_id, "repo_dir")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("repo_dir output not found".to_string())
            })?;
        let checkout_ref: String = context
            .get_output(download_job_id, "checkout_ref")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("checkout_ref output not found".to_string())
            })?;
        let repo_owner: String = context
            .get_output(download_job_id, "repo_owner")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("repo_owner output not found".to_string())
            })?;
        let repo_name: String = context
            .get_output(download_job_id, "repo_name")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("repo_name output not found".to_string())
            })?;

        Ok(Self {
            repo_dir: PathBuf::from(repo_dir_str),
            checkout_ref,
            repo_owner,
            repo_name,
        })
    }
}

/// Typed output from BuildImageJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOutput {
    pub image_tag: String,
    pub image_id: String,
    pub size_bytes: u64,
    pub build_context: PathBuf,
    pub dockerfile_path: PathBuf,
}

/// Configuration for building images
#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub dockerfile_path: Option<String>,
    pub build_context: Option<String>,
    pub build_args: Vec<(String, String)>,
    pub target_platform: Option<String>,
    pub cache_from: Vec<String>,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            dockerfile_path: Some("Dockerfile".to_string()),
            build_context: Some(".".to_string()),
            build_args: Vec::new(),
            target_platform: None,
            cache_from: Vec::new(),
        }
    }
}

/// Job for building container images from source code
pub struct BuildImageJob {
    job_id: String,
    download_job_id: String,
    image_tag: String,
    build_config: BuildConfig,
    image_builder: Arc<dyn ImageBuilder>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    preset: Option<String>, // Preset slug to generate Dockerfile if missing
}

impl std::fmt::Debug for BuildImageJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildImageJob")
            .field("job_id", &self.job_id)
            .field("download_job_id", &self.download_job_id)
            .field("image_tag", &self.image_tag)
            .field("build_config", &self.build_config)
            .field("image_builder", &"<ImageBuilder>")
            .finish()
    }
}

impl BuildImageJob {
    pub fn new(
        job_id: String,
        download_job_id: String,
        image_tag: String,
        image_builder: Arc<dyn ImageBuilder>,
    ) -> Self {
        Self {
            job_id,
            download_job_id,
            image_tag,
            build_config: BuildConfig::default(),
            image_builder,
            log_id: None,
            log_service: None,
            preset: None,
        }
    }

    pub fn with_build_config(mut self, build_config: BuildConfig) -> Self {
        self.build_config = build_config;
        self
    }

    pub fn with_dockerfile_path(mut self, dockerfile_path: String) -> Self {
        self.build_config.dockerfile_path = Some(dockerfile_path);
        self
    }

    pub fn with_build_args(mut self, build_args: Vec<(String, String)>) -> Self {
        self.build_config.build_args = build_args;
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

    pub fn with_preset(mut self, preset: String) -> Self {
        self.preset = Some(preset);
        self
    }

    /// Write log message to job-specific log file
    /// Write log message to both job-specific log file and context log writer
    async fn log(&self, context: &WorkflowContext, message: String) -> Result<(), WorkflowError> {
        // Write to job-specific log file
        if let (Some(ref log_id), Some(ref log_service)) = (&self.log_id, &self.log_service) {
            log_service
                .append_to_log(log_id, &format!("{}\n", message))
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        // Also write to context log writer (for real-time streaming and test capture)
        context.log(&message).await?;
        Ok(())
    }

    /// Generate Dockerfile from preset if it doesn't exist
    /// Returns the build args from the preset (if any)
    ///
    /// # Arguments
    /// * `context` - Workflow context for logging
    /// * `build_context_dir` - The directory that will be used as Docker build context (where to generate/look for Dockerfile)
    /// * `dockerfile_path` - Full path where Dockerfile should be generated
    /// Generate framework-specific nixpacks.toml configuration
    ///
    /// This method detects the Node.js framework being used (Astro, Vite, Next.js, etc.)
    /// and generates an optimized nixpacks.toml with framework-specific start commands.
    /// Only generates the file if:
    /// 1. package.json exists (Node.js project)
    /// 2. No custom nixpacks.toml already exists
    /// 3. Framework has specific configuration (not all frameworks need overrides)
    async fn generate_framework_specific_nixpacks_config(
        &self,
        context: &WorkflowContext,
        build_context_dir: &PathBuf,
    ) -> Result<(), WorkflowError> {
        let nixpacks_toml_path = build_context_dir.join("nixpacks.toml");
        let package_json_path = build_context_dir.join("package.json");

        // Skip if nixpacks.toml already exists (user provided)
        if nixpacks_toml_path.exists() {
            self.log(
                context,
                "ℹ️  Custom nixpacks.toml found, skipping framework detection".to_string(),
            )
            .await?;
            return Ok(());
        }

        // Skip if not a Node.js project
        if !package_json_path.exists() {
            return Ok(());
        }

        // Detect framework
        let framework = temps_presets::detect_node_framework(build_context_dir);

        self.log(
            context,
            format!("🔍 Detected Node.js framework: {}", framework.name()),
        )
        .await?;

        // Generate nixpacks.toml if framework has specific configuration
        if let Some(config) = framework.nixpacks_config() {
            fs::write(&nixpacks_toml_path, config).map_err(WorkflowError::IoError)?;

            self.log(
                context,
                format!(
                    "✅ Generated framework-specific nixpacks.toml for {}",
                    framework.name()
                ),
            )
            .await?;
        } else {
            self.log(
                context,
                format!(
                    "ℹ️  {} uses default nixpacks configuration",
                    framework.name()
                ),
            )
            .await?;
        }

        Ok(())
    }

    async fn ensure_dockerfile(
        &self,
        context: &WorkflowContext,
        build_context_dir: &PathBuf,
        dockerfile_path: &PathBuf,
    ) -> Result<std::collections::HashMap<String, String>, WorkflowError> {
        // If Dockerfile exists, we're done (no preset build args)
        if dockerfile_path.exists() {
            return Ok(std::collections::HashMap::new());
        }

        // Determine preset: either use provided slug or auto-detect
        let preset_slug = if let Some(slug) = &self.preset {
            // Use provided preset
            self.log(
                context,
                format!("📝 Dockerfile not found, generating from preset: {}", slug),
            )
            .await?;
            slug.clone()
        } else {
            // Auto-detect preset from project files
            self.log(
                context,
                "🔍 No preset specified, auto-detecting project type...".to_string(),
            )
            .await?;

            // Read directory to get list of files
            let files: Vec<String> = fs::read_dir(build_context_dir)
                .map_err(WorkflowError::IoError)?
                .filter_map(|entry| {
                    entry
                        .ok()
                        .and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
                })
                .collect();

            // Try to read package.json for more accurate detection
            let package_json_path = build_context_dir.join("package.json");
            let package_json_content = if package_json_path.exists() {
                fs::read_to_string(&package_json_path).ok()
            } else {
                None
            };

            // Check for Create React App by looking for react-scripts in package.json
            let detected_slug = if let Some(content) = &package_json_content {
                if content.contains("\"react-scripts\"") {
                    self.log(
                        context,
                        "✅ Detected project type: react-app (found react-scripts in package.json)"
                            .to_string(),
                    )
                    .await?;
                    "react-app".to_string()
                } else {
                    // Fall back to file-based detection
                    let detected_preset = temps_presets::detect_preset_from_files(&files)
                        .ok_or_else(|| {
                            WorkflowError::JobExecutionFailed(
                                format!("Could not auto-detect project type from files: {:?}. Please specify a preset explicitly.",
                                files.iter().take(5).collect::<Vec<_>>())
                            )
                        })?;

                    let slug = detected_preset.slug().to_string();
                    self.log(context, format!("✅ Detected project type: {}", slug))
                        .await?;
                    slug
                }
            } else {
                // No package.json, use file-based detection
                let detected_preset = temps_presets::detect_preset_from_files(&files)
                    .ok_or_else(|| {
                        WorkflowError::JobExecutionFailed(
                            format!("Could not auto-detect project type from files: {:?}. Please specify a preset explicitly.",
                            files.iter().take(5).collect::<Vec<_>>())
                        )
                    })?;

                let slug = detected_preset.slug().to_string();
                self.log(context, format!("✅ Detected project type: {}", slug))
                    .await?;
                slug
            };

            detected_slug
        };

        // Get the preset
        let preset = temps_presets::get_preset_by_slug(&preset_slug).ok_or_else(|| {
            WorkflowError::JobExecutionFailed(format!("Unknown preset: {}", preset_slug))
        })?;

        // Convert build args to build_vars format (Vec<String> of "KEY" for ARG directives)
        let build_vars: Vec<String> = self
            .build_config
            .build_args
            .iter()
            .map(|(key, _)| key.clone())
            .collect();

        // Generate Dockerfile content with build args
        // Use build_context_dir as both root and local path so preset detection works correctly
        // TODO: Get use_buildkit from ImageBuilder configuration
        let dockerfile_with_args = preset
            .dockerfile(temps_presets::DockerfileConfig {
                root_local_path: build_context_dir,
                local_path: build_context_dir,
                install_command: None,         // auto-detect
                build_command: None,           // auto-detect
                output_dir: None,              // auto-detect
                build_vars: Some(&build_vars), // ARG directives for env vars
                project_slug: "deployment",
                use_buildkit: true, // Enable BuildKit for faster builds and caching
            })
            .await;

        // Write the Dockerfile
        fs::write(dockerfile_path, &dockerfile_with_args.content)
            .map_err(WorkflowError::IoError)?;

        self.log(
            context,
            format!(
                "✅ Generated Dockerfile at: {} ({} build args from preset)",
                dockerfile_path.display(),
                dockerfile_with_args.build_args.len()
            ),
        )
        .await?;

        // If using nixpacks preset, detect framework and generate nixpacks.toml if needed
        if preset_slug.starts_with("nixpacks") {
            self.generate_framework_specific_nixpacks_config(context, build_context_dir)
                .await?;
        }

        // Return the preset build args so the caller can merge them
        Ok(dockerfile_with_args.build_args)
    }

    /// Build the container image with real-time logging
    async fn build_image(
        &self,
        repo_output: &RepositoryOutput,
        context: &WorkflowContext,
    ) -> Result<ImageOutput, WorkflowError> {
        self.log(
            context,
            format!("🐳 Starting image build for {}", self.image_tag),
        )
        .await?;

        // Determine build context first (needed for Dockerfile path)
        let build_context = if let Some(ref context_path) = self.build_config.build_context {
            repo_output.repo_dir.join(context_path)
        } else {
            repo_output.repo_dir.clone()
        };

        // Determine dockerfile path relative to build context
        let dockerfile_path = if let Some(ref dockerfile) = self.build_config.dockerfile_path {
            build_context.join(dockerfile)
        } else {
            build_context.join("Dockerfile")
        };

        self.log(
            context,
            format!("📄 Using Dockerfile: {}", dockerfile_path.display()),
        )
        .await?;

        // Ensure Dockerfile exists (generate from preset if needed)
        // This returns build args from the preset
        let preset_build_args = self
            .ensure_dockerfile(context, &build_context, &dockerfile_path)
            .await?;

        // Merge preset build args with user-provided build args
        // User-provided args take precedence
        let user_arg_keys: std::collections::HashSet<String> = self
            .build_config
            .build_args
            .iter()
            .map(|(k, _)| k.clone())
            .collect();

        let mut build_args = self.build_config.build_args.clone();
        for (key, value) in preset_build_args {
            if !user_arg_keys.contains(&key) {
                build_args.push((key, value));
            }
        }

        self.log(
            context,
            format!("📁 Build context: {}", build_context.display()),
        )
        .await?;

        // Create a temporary log file for the build
        let log_path = std::env::temp_dir().join(format!("build_{}.log", self.job_id));

        // Build the image using ImageBuilder trait
        self.log(context, "🔨 Building container image...".to_string())
            .await?;

        let mut build_args = HashMap::new();
        for (key, value) in &self.build_config.build_args {
            build_args.insert(key.clone(), value.clone());
        }

        let build_request = BuildRequest {
            image_name: self.image_tag.clone(),
            context_path: build_context.clone(),
            dockerfile_path: Some(dockerfile_path.clone()),
            build_args,
            platform: self.build_config.target_platform.clone(),
            log_path: log_path.clone(),
        };

        // Create log callback to stream Docker build output to job logs
        let log_service = self.log_service.clone();
        let log_id = self.log_id.clone();
        let log_callback: Option<temps_deployer::LogCallback> =
            if let (Some(log_svc), Some(log_id_str)) = (log_service, log_id) {
                Some(std::sync::Arc::new(move |line: String| {
                    let log_svc_clone = log_svc.clone();
                    let log_id_clone = log_id_str.clone();
                    Box::pin(async move {
                        let _ = log_svc_clone.append_to_log(&log_id_clone, &line).await;
                    })
                }))
            } else {
                None
            };

        let build_request_with_callback = temps_deployer::BuildRequestWithCallback {
            request: build_request,
            log_callback,
        };

        let build_result = self
            .image_builder
            .build_image_with_callback(build_request_with_callback)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!("Failed to build image: {}", e))
            })?;

        self.log(
            context,
            format!(
                "✅ Image built successfully: {} ({})",
                build_result.image_name, build_result.image_id
            ),
        )
        .await?;
        self.log(
            context,
            format!(
                "📊 Image size: {} MB",
                build_result.size_bytes / (1024 * 1024)
            ),
        )
        .await?;
        self.log(
            context,
            format!("⏱️  Build time: {} ms", build_result.build_duration_ms),
        )
        .await?;

        Ok(ImageOutput {
            image_tag: build_result.image_name,
            image_id: build_result.image_id,
            size_bytes: build_result.size_bytes,
            build_context,
            dockerfile_path,
        })
    }
}

#[async_trait]
impl WorkflowTask for BuildImageJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Build Image"
    }

    fn description(&self) -> &str {
        "Builds a container image from repository source code"
    }

    fn depends_on(&self) -> Vec<String> {
        vec![self.download_job_id.clone()]
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        // Get typed output from the download job
        let repo_output = RepositoryOutput::from_context(&context, &self.download_job_id)?;

        // Build the image (logs written in real-time)
        let image_output = self.build_image(&repo_output, &context).await?;

        // Set typed job outputs
        context.set_output(&self.job_id, "image_tag", &image_output.image_tag)?;
        context.set_output(&self.job_id, "image_id", &image_output.image_id)?;
        context.set_output(&self.job_id, "size_bytes", image_output.size_bytes)?;
        context.set_output(
            &self.job_id,
            "build_context",
            image_output.build_context.to_string_lossy().to_string(),
        )?;
        context.set_output(
            &self.job_id,
            "dockerfile_path",
            image_output.dockerfile_path.to_string_lossy().to_string(),
        )?;

        // Set artifacts
        context.set_artifact(
            &self.job_id,
            "container_image",
            PathBuf::from(&image_output.image_tag),
        );

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Verify that the download job output is available
        RepositoryOutput::from_context(context, &self.download_job_id)?;

        // Basic validation
        if self.image_tag.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "image_tag cannot be empty".to_string(),
            ));
        }
        if self.download_job_id.is_empty() {
            return Err(WorkflowError::JobValidationFailed(
                "download_job_id cannot be empty".to_string(),
            ));
        }

        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        // Container images persist beyond job completion
        // Could implement cleanup logic here if needed (e.g., remove intermediate layers)
        Ok(())
    }
}

/// Builder for BuildImageJob
pub struct BuildImageJobBuilder {
    job_id: Option<String>,
    download_job_id: Option<String>,
    image_tag: Option<String>,
    build_config: BuildConfig,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
    preset: Option<String>,
}

impl BuildImageJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            download_job_id: None,
            image_tag: None,
            build_config: BuildConfig::default(),
            log_id: None,
            log_service: None,
            preset: None,
        }
    }

    pub fn job_id(mut self, job_id: String) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn download_job_id(mut self, download_job_id: String) -> Self {
        self.download_job_id = Some(download_job_id);
        self
    }

    pub fn image_tag(mut self, image_tag: String) -> Self {
        self.image_tag = Some(image_tag);
        self
    }

    pub fn dockerfile_path(mut self, dockerfile_path: String) -> Self {
        self.build_config.dockerfile_path = Some(dockerfile_path);
        self
    }

    pub fn build_context(mut self, build_context: String) -> Self {
        self.build_config.build_context = Some(build_context);
        self
    }

    pub fn build_args(mut self, build_args: Vec<(String, String)>) -> Self {
        self.build_config.build_args = build_args;
        self
    }

    pub fn target_platform(mut self, target_platform: String) -> Self {
        self.build_config.target_platform = Some(target_platform);
        self
    }

    pub fn cache_from(mut self, cache_from: Vec<String>) -> Self {
        self.build_config.cache_from = cache_from;
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

    pub fn preset(mut self, preset: String) -> Self {
        self.preset = Some(preset);
        self
    }

    pub fn build(
        self,
        image_builder: Arc<dyn ImageBuilder>,
    ) -> Result<BuildImageJob, WorkflowError> {
        let job_id = self.job_id.unwrap_or_else(|| "build_image".to_string());
        let download_job_id = self.download_job_id.ok_or_else(|| {
            WorkflowError::JobValidationFailed("download_job_id is required".to_string())
        })?;
        let image_tag = self.image_tag.ok_or_else(|| {
            WorkflowError::JobValidationFailed("image_tag is required".to_string())
        })?;

        let mut job = BuildImageJob::new(job_id, download_job_id, image_tag, image_builder)
            .with_build_config(self.build_config);

        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
        }
        if let Some(preset) = self.preset {
            job = job.with_preset(preset);
        }

        Ok(job)
    }
}

impl Default for BuildImageJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::Path;

    use temps_deployer::{
        BuildRequest, BuildRequestWithCallback, BuildResult, BuilderError, ImageBuilder,
    };

    // Mock ImageBuilder for testing
    struct MockImageBuilder;

    #[async_trait]
    impl ImageBuilder for MockImageBuilder {
        async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
            Ok(BuildResult {
                image_id: "sha256:test123".to_string(),
                image_name: request.image_name,
                size_bytes: 104857600, // 100MB
                build_duration_ms: 5000,
            })
        }

        async fn import_image(
            &self,
            _image_path: PathBuf,
            _tag: &str,
        ) -> Result<String, BuilderError> {
            Ok("sha256:imported".to_string())
        }

        async fn extract_from_image(
            &self,
            _image_name: &str,
            _source_path: &str,
            _destination_path: &Path,
        ) -> Result<(), BuilderError> {
            Ok(())
        }

        async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
            Ok(vec!["test:latest".to_string()])
        }

        async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
            Ok(())
        }

        async fn build_image_with_callback(
            &self,
            request: BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            // Delegate to regular build_image since we don't need callback in tests
            self.build_image(request.request).await
        }
    }

    #[test]
    fn test_build_image_job_builder() {
        let image_builder: Arc<dyn ImageBuilder> = Arc::new(MockImageBuilder);

        let job = BuildImageJobBuilder::new()
            .job_id("test_build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .dockerfile_path("docker/Dockerfile".to_string())
            .build_args(vec![("ENV".to_string(), "production".to_string())])
            .build(image_builder)
            .unwrap();

        assert_eq!(job.job_id(), "test_build");
        assert_eq!(job.download_job_id, "download_repo");
        assert_eq!(job.image_tag, "myapp:latest");
        assert_eq!(job.depends_on(), vec!["download_repo".to_string()]);
    }

    #[test]
    fn test_repository_output_from_context() {
        let mut context = crate::test_utils::create_test_context("test".to_string(), 1, 1, 1);

        // Set up outputs as the download job would
        context
            .set_output("download_repo", "repo_dir", "/tmp/repo")
            .unwrap();
        context
            .set_output("download_repo", "checkout_ref", "main")
            .unwrap();
        context
            .set_output("download_repo", "repo_owner", "user")
            .unwrap();
        context
            .set_output("download_repo", "repo_name", "project")
            .unwrap();

        let repo_output = RepositoryOutput::from_context(&context, "download_repo").unwrap();
        assert_eq!(repo_output.repo_dir, PathBuf::from("/tmp/repo"));
        assert_eq!(repo_output.checkout_ref, "main");
        assert_eq!(repo_output.repo_owner, "user");
        assert_eq!(repo_output.repo_name, "project");
    }
}
