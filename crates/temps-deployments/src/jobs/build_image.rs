//! Build Image Job
//!
//! Builds container images from downloaded repository source code

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use temps_core::{
    JobResult, TempsConfig, WorkflowCancellationProvider, WorkflowContext, WorkflowError,
    WorkflowTask,
};
use temps_deployer::{BuildRequest, ImageBuilder};
use temps_entities::preset::{Preset as StoredPreset, PresetConfig as StoredPresetConfig};
use temps_logs::{LogLevel, LogService};
use temps_presets;
use tokio::time::{sleep, Duration};

fn validate_relative_build_path(path: &Path, label: &str) -> Result<(), WorkflowError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(WorkflowError::JobValidationFailed(format!(
            "{label} '{}' must be relative and contained by the build context",
            path.display()
        )));
    }
    Ok(())
}

fn validate_confined_regular_file(root: &Path, path: &Path) -> Result<(), WorkflowError> {
    let metadata = fs::symlink_metadata(path).map_err(WorkflowError::IoError)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build input '{}' must be a regular non-symlink file",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(WorkflowError::IoError)?;
    if !canonical.starts_with(root) {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build input '{}' escapes build context '{}'",
            canonical.display(),
            root.display()
        )));
    }
    Ok(())
}

fn write_no_follow(path: &Path, contents: &[u8], create_new: bool) -> Result<(), WorkflowError> {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(WorkflowError::IoError)?;
    if !file.metadata().map_err(WorkflowError::IoError)?.is_file() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build output '{}' must be a regular file",
            path.display()
        )));
    }
    file.write_all(contents).map_err(WorkflowError::IoError)
}

fn read_confined_control_file(
    root: &Path,
    path: &Path,
    max_bytes: u64,
) -> Result<Option<String>, WorkflowError> {
    let canonical_root = root.canonicalize().map_err(WorkflowError::IoError)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(WorkflowError::IoError(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' must be a regular non-symlink file",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(WorkflowError::IoError)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' escapes build context '{}'",
            canonical.display(),
            canonical_root.display()
        )));
    }
    if metadata.len() > max_bytes {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' exceeds the {max_bytes} byte limit",
            path.display()
        )));
    }
    let mut contents = String::new();
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(WorkflowError::IoError)?;
    if !file.metadata().map_err(WorkflowError::IoError)?.is_file() {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' must remain a regular file",
            path.display()
        )));
    }
    file.take(max_bytes + 1)
        .read_to_string(&mut contents)
        .map_err(WorkflowError::IoError)?;
    if contents.len() as u64 > max_bytes {
        return Err(WorkflowError::JobValidationFailed(format!(
            "Build control file '{}' exceeds the {max_bytes} byte limit",
            path.display()
        )));
    }
    Ok(Some(contents))
}

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
    /// Every tag this build produced, keyed by canonical platform.
    ///
    /// Empty for a single-platform build (the default): `image_tag` is then
    /// the whole story. On a heterogeneous cluster it holds one entry per
    /// architecture — the first platform keeps the plain `image_tag`, the rest
    /// get `-<arch>` suffixed tags — and the deploy job picks the entry that
    /// matches each node.
    #[serde(default)]
    pub image_tags_by_platform: HashMap<String, String>,
}

impl ImageOutput {
    /// Extract ImageOutput from WorkflowContext
    pub fn from_context(
        context: &WorkflowContext,
        build_job_id: &str,
    ) -> Result<Self, WorkflowError> {
        let image_tag: String =
            context
                .get_output(build_job_id, "image_tag")?
                .ok_or_else(|| {
                    WorkflowError::JobValidationFailed("image_tag output not found".to_string())
                })?;
        let image_id: String = context
            .get_output(build_job_id, "image_id")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("image_id output not found".to_string())
            })?;
        let size_bytes: u64 = context
            .get_output(build_job_id, "size_bytes")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("size_bytes output not found".to_string())
            })?;
        let build_context_str: String = context
            .get_output(build_job_id, "build_context")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("build_context output not found".to_string())
            })?;
        let dockerfile_path_str: String = context
            .get_output(build_job_id, "dockerfile_path")?
            .ok_or_else(|| {
                WorkflowError::JobValidationFailed("dockerfile_path output not found".to_string())
            })?;

        let image_tags_by_platform: HashMap<String, String> = context
            .get_output(build_job_id, "image_tags_by_platform")?
            .unwrap_or_default();

        Ok(Self {
            image_tag,
            image_id,
            size_bytes,
            build_context: PathBuf::from(build_context_str),
            dockerfile_path: PathBuf::from(dockerfile_path_str),
            image_tags_by_platform,
        })
    }
}

/// Configuration for building images
#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub dockerfile_path: Option<String>,
    pub build_context: Option<String>,
    pub build_args: Vec<(String, String)>,
    pub build_args_buildkit: Vec<(String, String)>,
    /// Container platforms to build for, in priority order.
    ///
    /// Empty (the default) builds exactly once, on the daemon's native
    /// platform — identical to the behaviour before multi-arch support. When
    /// populated, the **first** entry produces the plain `image_tag` and each
    /// additional entry produces a `-<arch>` suffixed tag. Non-native entries
    /// are built through the daemon's `platform` option, which needs QEMU
    /// binfmt handlers registered on the host.
    pub target_platforms: Vec<String>,
    pub cache_from: Vec<String>,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            dockerfile_path: Some("Dockerfile".to_string()),
            build_context: Some(".".to_string()),
            build_args: Vec::new(),
            build_args_buildkit: Vec::new(),
            target_platforms: Vec::new(),
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
    preset: Option<StoredPreset>,
    preset_config: Option<StoredPresetConfig>,
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
            preset_config: None,
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

    pub fn with_build_args_buildkit(mut self, build_args_buildkit: Vec<(String, String)>) -> Self {
        self.build_config.build_args_buildkit = build_args_buildkit;
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

    pub fn with_preset(mut self, preset: StoredPreset) -> Self {
        self.preset = Some(preset);
        self
    }

    pub fn with_preset_config(mut self, preset_config: Option<StoredPresetConfig>) -> Self {
        self.preset_config = preset_config;
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

    /// Generate Dockerfile from preset if it doesn't exist
    /// Returns the build args from the preset (if any)
    ///
    /// # Arguments
    /// * `context` - Workflow context for logging
    /// * `build_context_dir` - The directory that will be used as Docker build context (where to generate/look for Dockerfile)
    /// * `dockerfile_path` - Full path where Dockerfile should be generated
    ///
    /// Generate framework-specific nixpacks.toml configuration
    ///
    /// This method detects the Node.js framework being used (Astro, Vite, Next.js, etc.)
    /// and generates an optimized nixpacks.toml with framework-specific start commands.
    /// Only generates the file if:
    ///
    /// 1. package.json exists (Node.js project)
    /// 2. No custom nixpacks.toml already exists
    /// 3. Framework has specific configuration (not all frameworks need overrides)
    async fn generate_framework_specific_nixpacks_config(
        &self,
        context: &WorkflowContext,
        build_context_dir: &Path,
    ) -> Result<(), WorkflowError> {
        let nixpacks_toml_path = build_context_dir.join("nixpacks.toml");
        let hidden_nixpacks_toml_path = build_context_dir.join(".nixpacks.toml");
        let package_json_path = build_context_dir.join("package.json");

        // Validate user-provided Nixpacks config before any host-side planner
        // can follow it, then preserve it unchanged.
        for config_path in [&nixpacks_toml_path, &hidden_nixpacks_toml_path] {
            if read_confined_control_file(build_context_dir, config_path, 1024 * 1024)?.is_some() {
                self.log(
                    context,
                    "Custom nixpacks.toml found, skipping framework detection".to_string(),
                )
                .await?;
                return Ok(());
            }
        }

        let Some(package_json) =
            read_confined_control_file(build_context_dir, &package_json_path, 5 * 1024 * 1024)?
        else {
            return Ok(());
        };

        // Detect framework
        let framework = temps_presets::detect_node_framework_from_package_json(&package_json);

        self.log(
            context,
            format!("Detected Node.js framework: {}", framework.name()),
        )
        .await?;

        // Generate nixpacks.toml if framework has specific configuration
        if let Some(config) = framework.nixpacks_config() {
            if let Ok(metadata) = fs::symlink_metadata(&nixpacks_toml_path) {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(WorkflowError::JobValidationFailed(format!(
                        "nixpacks config '{}' must be a regular non-symlink file",
                        nixpacks_toml_path.display()
                    )));
                }
            }
            write_no_follow(&nixpacks_toml_path, config.as_bytes(), false)?;

            self.log(
                context,
                format!(
                    "Generated framework-specific nixpacks.toml for {}",
                    framework.name()
                ),
            )
            .await?;
        } else {
            self.log(
                context,
                format!("{} uses default nixpacks configuration", framework.name()),
            )
            .await?;
        }

        Ok(())
    }

    /// Load and parse .temps.yaml from the build context directory.
    /// Returns None if the file does not exist or cannot be parsed.
    fn load_temps_config(
        &self,
        build_context_dir: &Path,
    ) -> Result<Option<TempsConfig>, WorkflowError> {
        let config_path = build_context_dir.join(".temps.yaml");
        let Some(contents) =
            read_confined_control_file(build_context_dir, &config_path, 1024 * 1024)?
        else {
            return Ok(None);
        };
        TempsConfig::from_yaml(&contents)
            .map(Some)
            .map_err(|error| {
                WorkflowError::JobValidationFailed(format!(
                    "Invalid .temps.yaml at '{}': {error}",
                    config_path.display()
                ))
            })
    }

    async fn ensure_dockerfile(
        &self,
        context: &WorkflowContext,
        build_context_dir: &PathBuf,
        dockerfile_path: &PathBuf,
    ) -> Result<std::collections::HashMap<String, String>, WorkflowError> {
        // If Dockerfile exists, we're done (no preset build args)
        if fs::symlink_metadata(dockerfile_path).is_ok() {
            return Ok(std::collections::HashMap::new());
        }

        // Resolve the canonical stored preset and typed config, or auto-detect
        // a catalog preset when the job did not receive an explicit selection.
        let preset = if let Some(stored_preset) = self.preset {
            let runtime_slug =
                temps_presets::runtime_slug(stored_preset, self.preset_config.as_ref());
            self.log(
                context,
                format!(
                    "Dockerfile not found, generating from preset: {}",
                    runtime_slug
                ),
            )
            .await?;

            temps_presets::get_preset_for_storage(stored_preset, self.preset_config.as_ref())
                .map_err(|error| WorkflowError::JobExecutionFailed(error.to_string()))?
                .ok_or_else(|| {
                    WorkflowError::JobExecutionFailed(format!(
                        "No build preset registered for stored preset '{}'",
                        stored_preset
                    ))
                })?
        } else {
            self.log(
                context,
                "No preset specified, auto-detecting project type...".to_string(),
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
            let package_json_content =
                read_confined_control_file(build_context_dir, &package_json_path, 5 * 1024 * 1024)?;

            // Check for Create React App by looking for react-scripts in package.json
            let detected_slug = if let Some(content) = &package_json_content {
                if content.contains("\"react-scripts\"") {
                    self.log(
                        context,
                        "Detected project type: react-app (found react-scripts in package.json)"
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
                    self.log(context, format!("Detected project type: {}", slug))
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
                self.log(context, format!("Detected project type: {}", slug))
                    .await?;
                slug
            };

            temps_presets::get_preset_by_slug(&detected_slug).ok_or_else(|| {
                WorkflowError::JobExecutionFailed(format!(
                    "Unknown detected preset: {}",
                    detected_slug
                ))
            })?
        };

        let preset_slug = preset.slug();

        // Convert build args to build_vars format (Vec<String> of "KEY" for ARG directives)
        let build_vars: Vec<String> = self
            .build_config
            .build_args
            .iter()
            .map(|(key, _)| key.clone())
            .collect();

        // Get repository output to extract repo name for project slug
        let repo_output = RepositoryOutput::from_context(context, &self.download_job_id)?;

        // Use repo name as project slug (sanitized: lowercase, hyphens to underscores)
        let project_slug = repo_output.repo_name.replace("-", "_").to_lowercase();

        // Load .temps.yaml for build overrides (install_command, build_command, output_dir)
        let temps_config = self.load_temps_config(build_context_dir)?;
        let build_overrides = temps_config.as_ref().and_then(|c| c.build.as_ref());

        let install_cmd_owned = build_overrides.and_then(|b| b.install_command.clone());
        let build_cmd_owned = build_overrides.and_then(|b| b.build_command.clone());
        let output_dir_owned = build_overrides.and_then(|b| b.output_dir.clone());

        if build_overrides.is_some() {
            self.log(
                context,
                format!(
                    "Found .temps.yaml build overrides: install={:?}, build={:?}, output_dir={:?}",
                    install_cmd_owned, build_cmd_owned, output_dir_owned
                ),
            )
            .await?;
        }

        // Generate Dockerfile content with build args and .temps.yaml overrides
        // Use build_context_dir as both root and local path so preset detection works correctly
        let dockerfile_with_args = preset
            .dockerfile(temps_presets::DockerfileConfig {
                root_local_path: build_context_dir,
                local_path: build_context_dir,
                install_command: install_cmd_owned.as_deref(),
                build_command: build_cmd_owned.as_deref(),
                output_dir: output_dir_owned.as_deref(),
                build_vars: Some(&build_vars), // ARG directives for env vars
                project_slug: &project_slug,
                use_buildkit: true, // Enable BuildKit for faster builds and caching
            })
            .await;

        // Write the Dockerfile
        write_no_follow(
            dockerfile_path,
            dockerfile_with_args.content.as_bytes(),
            true,
        )?;

        self.log(
            context,
            format!(
                "Generated Dockerfile at: {} ({} build args from preset)",
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

    /// Write a `.npmrc` file into the build context when the user provides
    /// `NPM_RC` or `NPM_TOKEN` as a build env var.
    ///
    /// Matches the Vercel behavior: `NPM_RC` wins over `NPM_TOKEN` when both
    /// are set. The file contents are never logged; only the source env var
    /// name ("NPM_RC" or "NPM_TOKEN") is logged.
    async fn ensure_npmrc(
        &self,
        context: &WorkflowContext,
        build_context_dir: &Path,
    ) -> Result<(), WorkflowError> {
        // `build_config.build_args` is the flat map of user env vars / build args
        // that workflow_planner.rs assembles for this job.
        let env: HashMap<String, String> = self
            .build_config
            .build_args
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let Some(plan) = super::npmrc::plan_npmrc(&env) else {
            return Ok(());
        };

        let npmrc_path = build_context_dir.join(".npmrc");
        if let Ok(metadata) = fs::symlink_metadata(&npmrc_path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(WorkflowError::JobValidationFailed(format!(
                    ".npmrc '{}' must be a regular non-symlink file",
                    npmrc_path.display()
                )));
            }
        }
        write_no_follow(&npmrc_path, plan.contents.as_bytes(), false).map_err(|e| {
            WorkflowError::JobExecutionFailed(format!(
                "Failed to write .npmrc to {}: {}",
                npmrc_path.display(),
                e
            ))
        })?;

        self.log(
            context,
            format!(
                "Generated .npmrc from {} env var at {}",
                plan.source.as_str(),
                npmrc_path.display()
            ),
        )
        .await?;

        Ok(())
    }

    /// Build the container image with real-time logging
    async fn build_image(
        &self,
        repo_output: &RepositoryOutput,
        context: &WorkflowContext,
    ) -> Result<ImageOutput, WorkflowError> {
        self.log(
            context,
            format!("Starting image build for {}", self.image_tag),
        )
        .await?;

        // Determine build context first (needed for Dockerfile path)
        let build_context = if let Some(ref context_path) = self.build_config.build_context {
            let context_path = Path::new(context_path);
            if context_path.is_absolute()
                || context_path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                return Err(WorkflowError::JobValidationFailed(format!(
                    "Build context '{}' must be relative and contained by the source root",
                    context_path.display()
                )));
            }
            repo_output.repo_dir.join(context_path)
        } else {
            repo_output.repo_dir.clone()
        };

        let canonical_root = repo_output
            .repo_dir
            .canonicalize()
            .map_err(WorkflowError::IoError)?;
        let canonical_context = build_context
            .canonicalize()
            .map_err(WorkflowError::IoError)?;
        if !canonical_context.starts_with(&canonical_root) {
            return Err(WorkflowError::JobValidationFailed(format!(
                "Build context '{}' escapes source root '{}'",
                canonical_context.display(),
                canonical_root.display()
            )));
        }

        // Determine dockerfile path relative to build context
        let dockerfile_relative = self
            .build_config
            .dockerfile_path
            .as_deref()
            .map(Path::new)
            .unwrap_or_else(|| Path::new("Dockerfile"));
        validate_relative_build_path(dockerfile_relative, "Dockerfile path")?;
        let dockerfile_path = build_context.join(dockerfile_relative);
        let dockerfile_parent = dockerfile_path.parent().ok_or_else(|| {
            WorkflowError::JobValidationFailed("Dockerfile path has no parent".to_string())
        })?;
        let canonical_parent = dockerfile_parent
            .canonicalize()
            .map_err(WorkflowError::IoError)?;
        if !canonical_parent.starts_with(&canonical_context) {
            return Err(WorkflowError::JobValidationFailed(format!(
                "Dockerfile parent '{}' escapes build context '{}'",
                canonical_parent.display(),
                canonical_context.display()
            )));
        }
        if fs::symlink_metadata(&dockerfile_path).is_ok() {
            validate_confined_regular_file(&canonical_context, &dockerfile_path)?;
        }

        self.log(
            context,
            format!("Using Dockerfile: {}", dockerfile_path.display()),
        )
        .await?;

        // Ensure Dockerfile exists (generate from preset if needed)
        // This returns build args from the preset
        let preset_build_args = self
            .ensure_dockerfile(context, &build_context, &dockerfile_path)
            .await?;

        // Write .npmrc into the build context when NPM_RC / NPM_TOKEN env vars
        // are provided (Vercel-compatible behavior). No-op otherwise.
        self.ensure_npmrc(context, &build_context).await?;

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
            format!("Build context: {}", build_context.display()),
        )
        .await?;

        // Create a temporary log file for the build
        let log_path = std::env::temp_dir().join(format!("build_{}.log", self.job_id));

        // Build the image using ImageBuilder trait
        self.log(context, "Building container image...".to_string())
            .await?;

        let mut build_args = HashMap::new();
        for (key, value) in &self.build_config.build_args {
            build_args.insert(key.clone(), value.clone());
        }

        let mut build_args_buildkit = HashMap::new();
        for (key, value) in &self.build_config.build_args_buildkit {
            build_args_buildkit.insert(key.clone(), value.clone());
        }

        // Create log callback to stream Docker build output to job logs with structured logging
        let log_service = self.log_service.clone();
        let log_id = self.log_id.clone();
        let log_callback: Option<temps_deployer::LogCallback> =
            if let (Some(log_svc), Some(log_id_str)) = (log_service, log_id) {
                Some(std::sync::Arc::new(move |line: String| {
                    let log_svc_clone = log_svc.clone();
                    let log_id_clone = log_id_str.clone();
                    Box::pin(async move {
                        // Detect log level from Docker build output
                        let level = Self::detect_log_level(&line);
                        let _ = log_svc_clone
                            .append_structured_log(&log_id_clone, level, line)
                            .await;
                    })
                }))
            } else {
                None
            };

        // One build per requested platform. The empty case (`None` platform)
        // is the single-architecture path every existing deployment takes.
        let platforms: Vec<Option<String>> = if self.build_config.target_platforms.is_empty() {
            vec![None]
        } else {
            self.build_config
                .target_platforms
                .iter()
                .map(|p| Some(p.clone()))
                .collect()
        };

        let mut image_tags_by_platform: HashMap<String, String> = HashMap::new();
        let mut primary: Option<temps_deployer::BuildResult> = None;

        for (index, platform) in platforms.iter().enumerate() {
            // The first platform owns the plain tag; the rest are suffixed so
            // several architectures can coexist in one Docker image store
            // without a registry or manifest list.
            let tag = match platform {
                Some(platform) if index > 0 => format!(
                    "{}-{}",
                    self.image_tag,
                    temps_deployer::platform::platform_tag_suffix(platform)
                ),
                _ => self.image_tag.clone(),
            };

            if let Some(platform) = platform {
                self.log(
                    context,
                    format!("Building '{}' for platform {}...", tag, platform),
                )
                .await?;
            }

            let build_request = BuildRequest {
                image_name: tag.clone(),
                context_path: build_context.clone(),
                dockerfile_path: Some(dockerfile_path.clone()),
                build_args: build_args.clone(),
                build_args_buildkit: build_args_buildkit.clone(),
                platform: platform.clone(),
                log_path: log_path.clone(),
            };

            let build_request_with_callback = temps_deployer::BuildRequestWithCallback {
                request: build_request,
                log_callback: log_callback.clone(),
            };

            let build_result = match self
                .image_builder
                .build_image_with_callback(build_request_with_callback)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    // The *daemon's* platform, not the binary's: with a
                    // cross-architecture DOCKER_HOST those differ, and using
                    // the binary's would either recommend QEMU for a
                    // daemon-native build or omit that advice for a real
                    // cross-build.
                    let build_host_platform = self.image_builder.get_native_platform();
                    let message =
                        Self::describe_build_failure(platform.as_deref(), &build_host_platform, &e);

                    // Only the primary build is fatal. A secondary platform
                    // failing — most often because QEMU isn't installed for it
                    // — must not take the whole cluster's deployments down: it
                    // simply isn't in `image_tags_by_platform`, so the
                    // scheduler's architecture filter excludes those nodes and
                    // the deployment proceeds on the rest (or fails with a
                    // clean `NoCompatibleNode` if there is no rest).
                    if index == 0 {
                        self.log(context, format!("ERROR: {}", message)).await?;
                        return Err(WorkflowError::JobExecutionFailed(message));
                    }

                    self.log(
                        context,
                        format!(
                            "WARNING: {} Nodes running {} will be excluded from this deployment.",
                            message,
                            platform.as_deref().unwrap_or("that platform")
                        ),
                    )
                    .await?;
                    continue;
                }
            };

            self.log(
                context,
                format!(
                    "Image built successfully: {} ({})",
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
                format!("Build time: {} ms", build_result.build_duration_ms),
            )
            .await?;

            if let Some(platform) = platform {
                // Same rule: a mislabelled secondary image drops its platform
                // rather than failing every deployment in the cluster.
                match self
                    .verify_built_platform(&build_result.image_name, platform, context)
                    .await
                {
                    Ok(()) => {
                        image_tags_by_platform.insert(
                            temps_deployer::platform::canonicalize_platform(platform),
                            build_result.image_name.clone(),
                        );
                    }
                    Err(e) if index > 0 => {
                        // The tag exists but holds the wrong architecture.
                        // Leaving it behind would let a mislabelled image be
                        // picked up by hand later, so drop it — best-effort,
                        // since failing here would defeat the degradation.
                        if let Err(remove_err) = self
                            .image_builder
                            .remove_image(&build_result.image_name)
                            .await
                        {
                            tracing::debug!(
                                image = %build_result.image_name,
                                "Could not remove the mislabelled image: {}",
                                remove_err
                            );
                        }
                        self.log(
                            context,
                            format!(
                                "WARNING: {} Nodes running {} will be excluded from this \
                                 deployment.",
                                e, platform
                            ),
                        )
                        .await?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            if index == 0 {
                primary = Some(build_result);
            }
        }

        // `platforms` is never empty, so the first iteration always ran.
        let primary = primary.ok_or_else(|| {
            WorkflowError::JobExecutionFailed(
                "Internal error: image build produced no result".to_string(),
            )
        })?;

        Ok(ImageOutput {
            image_tag: primary.image_name,
            image_id: primary.image_id,
            size_bytes: primary.size_bytes,
            build_context,
            dockerfile_path,
            image_tags_by_platform,
        })
    }

    /// Confirm the image the daemon produced is actually for the platform we
    /// asked for.
    ///
    /// This is not paranoia: Docker's **legacy builder silently ignores**
    /// `platform`. It accepts the parameter, reports success, and hands back
    /// an image of the host's architecture — so a cross-build would produce
    /// `myapp:latest-arm64` containing amd64 binaries. Without this check the
    /// mislabelled image travels to the arm64 node and fails much later, with
    /// an error pointing at the node instead of at the build.
    ///
    /// BuildKit honours the platform correctly, so the fix is to enable it.
    ///
    /// An `inspect_image` that fails is not treated as a mismatch — some
    /// `ImageBuilder` implementations don't support inspection at all, and a
    /// missing check must not block an otherwise fine build.
    async fn verify_built_platform(
        &self,
        image_name: &str,
        requested_platform: &str,
        context: &WorkflowContext,
    ) -> Result<(), WorkflowError> {
        let built_platform = match self.image_builder.inspect_image(image_name).await {
            Ok(info) => info.platform,
            Err(e) => {
                tracing::debug!(
                    image = %image_name,
                    "Could not inspect the built image to verify its platform: {}",
                    e
                );
                return Ok(());
            }
        };

        if temps_deployer::platform::platforms_match(&built_platform, requested_platform) {
            return Ok(());
        }

        let msg = format!(
            "Build for {} produced a {} image ('{}'). The Docker daemon accepted the \
             requested platform but ignored it — this is what the legacy builder does. \
             Enable BuildKit on the control plane (Docker 23+ enables it by default; \
             otherwise set DOCKER_BUILDKIT=1 or install docker-buildx) and redeploy. \
             Deploying this image would fail on the target node with 'exec format error'.",
            requested_platform, built_platform, image_name
        );
        self.log(context, format!("ERROR: {}", msg)).await?;
        Err(WorkflowError::JobExecutionFailed(msg))
    }

    /// Turn a build failure into a message that says what to do about it.
    ///
    /// Cross-architecture builds go through the daemon's `platform` option,
    /// which silently requires QEMU binfmt handlers on the build host. Without
    /// them the failure surfaces as `exec format error` from inside the build —
    /// a message that reads like a broken Dockerfile and sends people looking
    /// in the wrong place. When we asked for a platform the build host doesn't
    /// run natively, say so and give the one command that fixes it.
    ///
    /// `build_host_platform` must be the **daemon's** platform (from
    /// `ImageBuilder::get_native_platform`), not this process's: they differ
    /// whenever `DOCKER_HOST` points at another machine, and the advice would
    /// then be aimed at the wrong host.
    fn describe_build_failure(
        platform: Option<&str>,
        build_host_platform: &str,
        error: &temps_deployer::BuilderError,
    ) -> String {
        let Some(platform) = platform else {
            return format!("Failed to build image: {}", error);
        };

        if temps_deployer::platform::platforms_match(platform, build_host_platform) {
            return format!("Failed to build image for {}: {}", platform, error);
        }

        let text = error.to_string().to_lowercase();
        let looks_like_missing_emulation = text.contains("exec format error")
            || text.contains("no match for platform")
            || text.contains("cannot execute binary file")
            || text.contains("unknown operating system or architecture");

        if looks_like_missing_emulation {
            format!(
                "Failed to build image for {} on a {} host: {}. \
                 Cross-architecture builds need QEMU emulation registered on the build host. \
                 Install it with: docker run --privileged --rm tonistiigi/binfmt --install {}. \
                 Alternatively, restrict this environment to {} nodes via target nodes/labels.",
                platform,
                build_host_platform,
                error,
                temps_deployer::platform::platform_arch(platform),
                build_host_platform
            )
        } else {
            format!("Failed to build image for {}: {}", platform, error)
        }
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
        // Only meaningful for a multi-arch build; an empty map downstream means
        // "one tag covers everything", which is what single-arch builds want.
        context.set_output(
            &self.job_id,
            "image_tags_by_platform",
            &image_output.image_tags_by_platform,
        )?;

        // Read .temps.yaml health config and pass it to downstream jobs
        // The DeployImageJob will use this to configure its health check path
        let build_context_dir = &image_output.build_context;
        if let Some(temps_config) = self.load_temps_config(build_context_dir)? {
            if let Some(health) = &temps_config.health {
                context.set_output(&self.job_id, "health_check_path", &health.path)?;
                context.set_output(&self.job_id, "health_check_timeout", health.timeout)?;
            }
        }

        // Set artifacts
        context.set_artifact(
            &self.job_id,
            "container_image",
            PathBuf::from(&image_output.image_tag),
        );

        Ok(JobResult::success(context))
    }

    async fn execute_with_cancellation(
        &self,
        context: WorkflowContext,
        cancellation_provider: &dyn WorkflowCancellationProvider,
    ) -> Result<JobResult, WorkflowError> {
        let workflow_run_id = context.workflow_run_id.clone();

        // Check if already cancelled before starting
        if cancellation_provider.is_cancelled(&workflow_run_id).await? {
            if let (Some(log_service), Some(log_id)) = (&self.log_service, &self.log_id) {
                log_service
                    .log_warning(
                        log_id,
                        "Build cancelled before starting - deployment was cancelled by user",
                    )
                    .await
                    .ok();
            }
            return Err(WorkflowError::BuildCancelled);
        }

        // Create cancellation check future that polls every 2 seconds
        let cancellation_check = async {
            loop {
                sleep(Duration::from_secs(2)).await;

                match cancellation_provider.is_cancelled(&workflow_run_id).await {
                    Ok(true) => {
                        // Cancellation detected
                        return;
                    }
                    Ok(false) => {
                        // Continue checking
                    }
                    Err(_) => {
                        // Error checking cancellation - stop polling
                        break;
                    }
                }
            }
        };

        // Race between build execution and cancellation detection
        let build_future = self.execute(context.clone());

        tokio::select! {
            result = build_future => {
                // Build completed (success or failure)
                result
            }
            _ = cancellation_check => {
                // Cancellation detected during build
                if let (Some(log_service), Some(log_id)) = (&self.log_service, &self.log_id) {
                    log_service
                        .log_warning(
                            log_id,
                            "🚫 Docker build cancelled by user - stopping image build",
                        )
                        .await
                        .ok();
                }

                Err(WorkflowError::BuildCancelled)
            }
        }
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
    preset: Option<StoredPreset>,
    preset_config: Option<StoredPresetConfig>,
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
            preset_config: None,
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

    pub fn build_args_buildkit(mut self, build_args_buildkit: Vec<(String, String)>) -> Self {
        self.build_config.build_args_buildkit = build_args_buildkit;
        self
    }

    pub fn target_platforms(mut self, target_platforms: Vec<String>) -> Self {
        self.build_config.target_platforms = target_platforms;
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

    pub fn preset(mut self, preset: StoredPreset) -> Self {
        self.preset = Some(preset);
        self
    }

    pub fn preset_config(mut self, preset_config: Option<StoredPresetConfig>) -> Self {
        self.preset_config = preset_config;
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
            .with_build_config(self.build_config.clone());

        if let Some(log_id) = self.log_id {
            job = job.with_log_id(log_id);
        }
        if let Some(log_service) = self.log_service {
            job = job.with_log_service(log_service);
        }
        if let Some(preset) = self.preset {
            job = job.with_preset(preset);
        }
        job = job.with_preset_config(self.preset_config);

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

        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, BuilderError> {
            Ok(temps_deployer::ImageInfo {
                id: "sha256:test123".to_string(),
                architecture: "amd64".to_string(),
                os: "linux".to_string(),
                platform: "linux/amd64".to_string(),
                size_bytes: 104857600,
                tags: vec!["test:latest".to_string()],
                created: None,
                working_dir: None,
            })
        }

        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &Path,
        ) -> Result<(), BuilderError> {
            Ok(())
        }

        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
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
    fn test_build_image_job_builder_preserves_typed_preset_config() {
        let image_builder: Arc<dyn ImageBuilder> = Arc::new(MockImageBuilder);
        let config = StoredPresetConfig::Nixpacks(temps_entities::preset::NixpacksConfig {
            nixpacks_config: None,
            providers: vec![
                temps_entities::preset::NixpacksProvider::Auto,
                temps_entities::preset::NixpacksProvider::Python,
            ],
        });

        let job = BuildImageJobBuilder::new()
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .preset(StoredPreset::Nixpacks)
            .preset_config(Some(config.clone()))
            .build(image_builder)
            .unwrap();

        assert_eq!(job.preset, Some(StoredPreset::Nixpacks));
        assert_eq!(job.preset_config, Some(config));
    }

    /// Records every platform the job asked for, so a test can assert what a
    /// multi-arch build actually did.
    #[derive(Default)]
    struct RecordingImageBuilder {
        builds: std::sync::Mutex<Vec<(String, Option<String>)>>,
        /// Platforms whose build should fail, with this error text.
        fail_platform: Option<(String, String)>,
    }

    impl RecordingImageBuilder {
        fn builds(&self) -> Vec<(String, Option<String>)> {
            self.builds.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ImageBuilder for RecordingImageBuilder {
        async fn build_image(&self, request: BuildRequest) -> Result<BuildResult, BuilderError> {
            self.builds
                .lock()
                .unwrap()
                .push((request.image_name.clone(), request.platform.clone()));

            if let (Some((fail_platform, message)), Some(requested)) =
                (self.fail_platform.as_ref(), request.platform.as_deref())
            {
                if fail_platform == requested {
                    return Err(BuilderError::BuildFailed(message.clone()));
                }
            }

            Ok(BuildResult {
                image_id: format!("sha256:{}", request.image_name),
                image_name: request.image_name,
                size_bytes: 1024 * 1024,
                build_duration_ms: 1,
            })
        }

        async fn build_image_with_callback(
            &self,
            request: BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            self.build_image(request.request).await
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
            Ok(vec![])
        }

        async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
            Ok(())
        }

        async fn inspect_image(
            &self,
            _image_name: &str,
        ) -> Result<temps_deployer::ImageInfo, BuilderError> {
            Err(BuilderError::ImageNotFound("not used in this test".into()))
        }

        async fn save_image(
            &self,
            _image_name: &str,
            _output_path: &Path,
        ) -> Result<(), BuilderError> {
            Ok(())
        }

        fn get_native_platform(&self) -> String {
            "linux/amd64".to_string()
        }
    }

    /// Build context with a trivial Dockerfile, so `build_image` doesn't try to
    /// generate one from a preset.
    fn repo_with_dockerfile() -> (tempfile::TempDir, RepositoryOutput) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        let repo = RepositoryOutput {
            repo_dir: dir.path().to_path_buf(),
            checkout_ref: "main".to_string(),
            repo_owner: "owner".to_string(),
            repo_name: "repo".to_string(),
        };
        (dir, repo)
    }

    #[tokio::test]
    async fn rejects_dockerfile_path_traversal() {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .dockerfile_path("../Dockerfile".to_string())
            .build(builder)
            .unwrap();
        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let error = job.build_image(&repo, &context).await.unwrap_err();
        assert!(matches!(error, WorkflowError::JobValidationFailed(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_dockerfile_and_npmrc_symlinks() {
        use std::os::unix::fs::symlink;

        let builder = Arc::new(RecordingImageBuilder::default());
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("outside");
        std::fs::write(&outside_file, "unchanged").unwrap();
        symlink(&outside_file, dir.path().join("Dockerfile")).unwrap();
        let repo = RepositoryOutput {
            repo_dir: dir.path().to_path_buf(),
            checkout_ref: "main".to_string(),
            repo_owner: "owner".to_string(),
            repo_name: "repo".to_string(),
        };
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build(builder.clone())
            .unwrap();
        assert!(matches!(
            job.build_image(&repo, &context).await,
            Err(WorkflowError::JobValidationFailed(_))
        ));

        std::fs::remove_file(dir.path().join("Dockerfile")).unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        symlink(&outside_file, dir.path().join(".npmrc")).unwrap();
        let npm_job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build_args(vec![("NPM_TOKEN".to_string(), "secret".to_string())])
            .build(builder)
            .unwrap();
        assert!(matches!(
            npm_job.build_image(&repo, &context).await,
            Err(WorkflowError::JobValidationFailed(_))
        ));
        assert_eq!(std::fs::read_to_string(outside_file).unwrap(), "unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_build_control_files_before_reading_them() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        for name in [
            "package.json",
            ".temps.yaml",
            "nixpacks.toml",
            ".nixpacks.toml",
        ] {
            let path = directory.path().join(name);
            symlink("/dev/zero", &path).unwrap();
            assert!(matches!(
                read_confined_control_file(directory.path(), &path, 1024),
                Err(WorkflowError::JobValidationFailed(_))
            ));
        }
    }

    /// Single-platform builds must keep behaving exactly as before: one build,
    /// the plain tag, no per-platform map.
    #[tokio::test]
    async fn test_build_without_target_platforms_builds_once() {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let output = job.build_image(&repo, &context).await.unwrap();

        assert_eq!(builder.builds(), vec![("myapp:latest".to_string(), None)]);
        assert_eq!(output.image_tag, "myapp:latest");
        assert!(output.image_tags_by_platform.is_empty());
    }

    /// A heterogeneous cluster builds one image per architecture. The first
    /// platform keeps the plain tag so single-arch consumers are unaffected;
    /// the rest are suffixed so both can coexist in one image store without a
    /// registry or manifest list.
    #[tokio::test]
    async fn test_multi_platform_build_produces_one_tag_per_platform() {
        let builder = Arc::new(RecordingImageBuilder::default());
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .target_platforms(vec!["linux/amd64".to_string(), "linux/arm64".to_string()])
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let output = job.build_image(&repo, &context).await.unwrap();

        assert_eq!(
            builder.builds(),
            vec![
                ("myapp:latest".to_string(), Some("linux/amd64".to_string())),
                (
                    "myapp:latest-arm64".to_string(),
                    Some("linux/arm64".to_string())
                ),
            ]
        );

        // The primary output stays the native tag...
        assert_eq!(output.image_tag, "myapp:latest");
        // ...and every platform is resolvable for the deploy job.
        assert_eq!(
            output.image_tags_by_platform.get("linux/amd64").unwrap(),
            "myapp:latest"
        );
        assert_eq!(
            output.image_tags_by_platform.get("linux/arm64").unwrap(),
            "myapp:latest-arm64"
        );
    }

    /// A secondary platform failing must NOT fail the deployment.
    ///
    /// `required_build_platforms` is driven by cluster topology, so an
    /// operator who joins an arm64 worker without installing QEMU would
    /// otherwise break every deployment in the cluster — strictly worse than
    /// the broken ARM replicas they had before. The platform drops out of
    /// `image_tags_by_platform` instead, and the scheduler's architecture
    /// filter excludes those nodes.
    #[tokio::test]
    async fn test_secondary_platform_failure_degrades_instead_of_aborting() {
        let builder = Arc::new(RecordingImageBuilder {
            builds: Default::default(),
            fail_platform: Some(("linux/arm64".to_string(), "exec format error".to_string())),
        });
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .target_platforms(vec!["linux/amd64".to_string(), "linux/arm64".to_string()])
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        let output = job
            .build_image(&repo, &context)
            .await
            .expect("the native build succeeded, so the deployment must proceed");

        // The native image is there and usable...
        assert_eq!(output.image_tag, "myapp:latest");
        assert_eq!(
            output.image_tags_by_platform.get("linux/amd64").unwrap(),
            "myapp:latest"
        );
        // ...and the platform that failed is absent, which is what makes the
        // scheduler exclude arm64 nodes rather than send them a broken image.
        assert!(
            !output.image_tags_by_platform.contains_key("linux/arm64"),
            "a failed platform must not be advertised: {:?}",
            output.image_tags_by_platform
        );
    }

    /// The primary build is still fatal — without it there is nothing to
    /// deploy anywhere.
    #[tokio::test]
    async fn test_primary_platform_failure_still_fails_the_job() {
        let builder = Arc::new(RecordingImageBuilder {
            builds: Default::default(),
            fail_platform: Some(("linux/amd64".to_string(), "boom".to_string())),
        });
        let job = BuildImageJobBuilder::new()
            .job_id("build".to_string())
            .download_job_id("download_repo".to_string())
            .image_tag("myapp:latest".to_string())
            .target_platforms(vec!["linux/amd64".to_string(), "linux/arm64".to_string()])
            .build(builder.clone())
            .unwrap();

        let (_dir, repo) = repo_with_dockerfile();
        let context = crate::test_utils::create_test_context("wf".to_string(), 1, 1, 1);

        assert!(job.build_image(&repo, &context).await.is_err());
    }

    #[test]
    fn test_describe_build_failure_mentions_emulation_only_for_cross_builds() {
        let host = "linux/amd64";

        // Native build: plain error, no misleading emulation advice.
        let native_msg = BuildImageJob::describe_build_failure(
            Some(host),
            host,
            &BuilderError::BuildFailed("exec format error".into()),
        );
        assert!(!native_msg.contains("binfmt"), "got: {}", native_msg);

        // Cross build failing for an unrelated reason: no emulation advice
        // either — don't send people chasing the wrong fix.
        let unrelated = BuildImageJob::describe_build_failure(
            Some("linux/arm64"),
            host,
            &BuilderError::BuildFailed("npm ERR! missing script: build".into()),
        );
        assert!(!unrelated.contains("binfmt"), "got: {}", unrelated);
        assert!(unrelated.contains("linux/arm64"), "got: {}", unrelated);

        // No platform requested at all (single-arch path).
        let plain = BuildImageJob::describe_build_failure(
            None,
            host,
            &BuilderError::BuildFailed("boom".into()),
        );
        assert_eq!(plain, "Failed to build image: Build failed: boom");
    }

    /// The advice must be aimed at the machine that runs the build. With a
    /// cross-architecture `DOCKER_HOST` the daemon's platform and this
    /// process's differ, and using the latter would recommend QEMU for a build
    /// the daemon runs natively — or stay silent about a genuine cross-build.
    #[test]
    fn test_describe_build_failure_judges_against_the_build_host_not_the_binary() {
        let emulation_failure = BuilderError::BuildFailed("exec format error".into());

        // Daemon is arm64 (via DOCKER_HOST); an arm64 build is native there,
        // whatever architecture this process was compiled for.
        let native_on_remote_daemon = BuildImageJob::describe_build_failure(
            Some("linux/arm64"),
            "linux/arm64",
            &emulation_failure,
        );
        assert!(
            !native_on_remote_daemon.contains("binfmt"),
            "a daemon-native build must not be blamed on missing emulation: {}",
            native_on_remote_daemon
        );

        // Same daemon, amd64 build: that IS a cross-build for it.
        let cross_on_remote_daemon = BuildImageJob::describe_build_failure(
            Some("linux/amd64"),
            "linux/arm64",
            &emulation_failure,
        );
        assert!(
            cross_on_remote_daemon.contains("binfmt"),
            "a real cross-build must carry the install command: {}",
            cross_on_remote_daemon
        );
        assert!(
            cross_on_remote_daemon.contains("--install amd64"),
            "the command must name the architecture to install: {}",
            cross_on_remote_daemon
        );
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
