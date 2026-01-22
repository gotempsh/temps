//! Build command for building Docker images locally
//!
//! This command builds Docker images locally using the Docker daemon,
//! optionally tagging and pushing them to a registry for deployment.

use clap::Args;
use colored::Colorize;
use std::path::PathBuf;

/// Build a Docker image locally
#[derive(Args)]
pub struct BuildCommand {
    /// Path to the build context (directory containing Dockerfile)
    #[arg(short, long, default_value = ".")]
    pub context: PathBuf,

    /// Path to the Dockerfile (relative to context)
    #[arg(short = 'f', long, default_value = "Dockerfile")]
    pub dockerfile: String,

    /// Image tag in the format repository:tag (e.g., myregistry.io/myapp:v1.0)
    #[arg(short, long)]
    pub tag: String,

    /// Additional image tags
    #[arg(long = "add-tag")]
    pub additional_tags: Vec<String>,

    /// Build arguments (can be specified multiple times)
    /// Format: KEY=VALUE
    #[arg(long = "build-arg")]
    pub build_args: Vec<String>,

    /// Target platform (e.g., linux/amd64, linux/arm64)
    #[arg(long)]
    pub platform: Option<String>,

    /// Push the image to the registry after building
    #[arg(long)]
    pub push: bool,

    /// Use BuildKit for building (default: auto-detect)
    #[arg(long)]
    pub buildkit: Option<bool>,

    /// Path to save build logs
    #[arg(long)]
    pub log_file: Option<PathBuf>,

    /// Don't print build output to console
    #[arg(short, long)]
    pub quiet: bool,

    /// Don't use cache when building the image
    #[arg(long)]
    pub no_cache: bool,

    /// Set build-time labels (can be specified multiple times)
    /// Format: KEY=VALUE
    #[arg(long = "label")]
    pub labels: Vec<String>,

    /// Output image digest after build
    #[arg(long)]
    pub output_digest: bool,
}

impl BuildCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        // Create a tokio runtime for async operations
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        rt.block_on(self.execute_async())
    }

    async fn execute_async(self) -> anyhow::Result<()> {
        use bollard::Docker;
        use futures::StreamExt;
        use std::collections::HashMap;

        println!("{}", "Temps Build".bright_blue().bold());
        println!();

        // Validate context path exists
        let context_path = self.context.canonicalize().map_err(|e| {
            anyhow::anyhow!(
                "Build context path '{}' does not exist: {}",
                self.context.display(),
                e
            )
        })?;

        // Check Dockerfile exists
        let dockerfile_path = context_path.join(&self.dockerfile);
        if !dockerfile_path.exists() {
            return Err(anyhow::anyhow!(
                "Dockerfile not found at: {}",
                dockerfile_path.display()
            ));
        }

        println!(
            "  {} Context: {}",
            "📁".bright_blue(),
            context_path.display()
        );
        println!("  {} Dockerfile: {}", "📄".bright_blue(), self.dockerfile);
        println!("  {} Image: {}", "🏷️".bright_blue(), self.tag);

        if !self.additional_tags.is_empty() {
            println!(
                "  {} Additional tags: {}",
                "🏷️".bright_blue(),
                self.additional_tags.join(", ")
            );
        }

        if let Some(ref platform) = self.platform {
            println!("  {} Platform: {}", "🖥️".bright_blue(), platform);
        }

        if !self.build_args.is_empty() {
            println!(
                "  {} Build args: {}",
                "⚙️".bright_blue(),
                self.build_args
                    .iter()
                    .map(|a| a.split('=').next().unwrap_or(a))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        println!();

        // Connect to Docker
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| anyhow::anyhow!("Failed to connect to Docker: {}", e))?;

        // Verify Docker is running
        docker
            .ping()
            .await
            .map_err(|e| anyhow::anyhow!("Docker daemon is not running: {}", e))?;

        println!("  {} Connected to Docker daemon", "✅".bright_green());

        // Create tar archive of build context
        println!(
            "  {} Creating build context archive...",
            "⏳".bright_yellow()
        );

        let tar_body = Self::create_tar_context(&context_path).await?;

        // Parse build arguments
        let mut build_args_map = HashMap::new();
        for arg in &self.build_args {
            if let Some((key, value)) = arg.split_once('=') {
                build_args_map.insert(key.to_string(), value.to_string());
            } else {
                return Err(anyhow::anyhow!(
                    "Invalid build argument format '{}'. Expected KEY=VALUE",
                    arg
                ));
            }
        }

        // Parse labels
        let mut labels_map = HashMap::new();
        labels_map.insert("built-by".to_string(), "temps-cli".to_string());
        for label in &self.labels {
            if let Some((key, value)) = label.split_once('=') {
                labels_map.insert(key.to_string(), value.to_string());
            } else {
                return Err(anyhow::anyhow!(
                    "Invalid label format '{}'. Expected KEY=VALUE",
                    label
                ));
            }
        }

        // Determine if we should use BuildKit
        let use_buildkit = match self.buildkit {
            Some(v) => v,
            None => {
                // Auto-detect: Try to check if BuildKit is available
                // For now, default to legacy builder for broader compatibility
                false
            }
        };

        // Configure build options
        let build_options = bollard::query_parameters::BuildImageOptions {
            dockerfile: self.dockerfile.clone(),
            t: Some(self.tag.clone()),
            buildargs: if build_args_map.is_empty() {
                None
            } else {
                Some(build_args_map)
            },
            labels: Some(labels_map),
            platform: self
                .platform
                .clone()
                .unwrap_or_else(Self::get_native_platform),
            nocache: self.no_cache,
            version: if use_buildkit {
                bollard::query_parameters::BuilderVersion::BuilderBuildKit
            } else {
                bollard::query_parameters::BuilderVersion::BuilderV1
            },
            session: if use_buildkit {
                Some(uuid::Uuid::new_v4().to_string())
            } else {
                None
            },
            ..Default::default()
        };

        println!("  {} Building image...", "🔨".bright_yellow());
        println!();

        // Open log file if specified
        let mut log_file = if let Some(ref log_path) = self.log_file {
            Some(
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(log_path)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to open log file: {}", e))?,
            )
        } else {
            None
        };

        // Start the build
        let mut build_stream = docker.build_image(
            build_options,
            None,
            Some(http_body_util::Either::Left(tar_body)),
        );

        let mut had_error = false;
        let start_time = std::time::Instant::now();

        // Stream build output
        while let Some(build_info) = build_stream.next().await {
            match build_info {
                Ok(info) => {
                    // Handle stream output (normal build messages)
                    if let Some(stream) = &info.stream {
                        let stream = stream.trim_end();
                        if !stream.is_empty() {
                            if !self.quiet {
                                println!("{}", stream.dimmed());
                            }
                            if let Some(ref mut file) = log_file {
                                use tokio::io::AsyncWriteExt;
                                let _ = file.write_all(stream.as_bytes()).await;
                                let _ = file.write_all(b"\n").await;
                            }
                        }
                    }

                    // Handle aux data (image ID) - extract from aux if available
                    // The aux field structure varies, so we'll get the image ID from list_images later
                    let _ = &info.aux; // Acknowledge aux but don't process it

                    // Handle error in build info
                    if let Some(ref error) = info.error {
                        eprintln!("  {} Build error: {}", "❌".bright_red(), error);
                        if let Some(ref mut file) = log_file {
                            use tokio::io::AsyncWriteExt;
                            let _ = file
                                .write_all(format!("ERROR: {}\n", error).as_bytes())
                                .await;
                        }
                        had_error = true;
                    }
                }
                Err(e) => {
                    eprintln!("  {} Build error: {}", "❌".bright_red(), e);
                    if let Some(ref mut file) = log_file {
                        use tokio::io::AsyncWriteExt;
                        let _ = file.write_all(format!("ERROR: {}\n", e).as_bytes()).await;
                    }
                    had_error = true;
                }
            }
        }

        if had_error {
            return Err(anyhow::anyhow!("Build failed"));
        }

        let build_duration = start_time.elapsed();

        println!();
        println!(
            "  {} Build completed in {:.1}s",
            "✅".bright_green(),
            build_duration.as_secs_f64()
        );

        // Get image info
        let images = docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("reference".to_string(), vec![self.tag.clone()]);
                    Some(filters)
                },
                ..Default::default()
            }))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get image info: {}", e))?;

        if let Some(image) = images.first() {
            let size_mb = image.size as f64 / 1024.0 / 1024.0;
            println!("  {} Image size: {:.1} MB", "📦".bright_blue(), size_mb);

            if self.output_digest {
                println!("  {} Image ID: {}", "🔑".bright_blue(), image.id);
            }
        }

        // Apply additional tags
        for additional_tag in &self.additional_tags {
            println!(
                "  {} Tagging as {}...",
                "🏷️".bright_yellow(),
                additional_tag
            );

            let (repo, tag) = Self::parse_image_ref(additional_tag)?;

            docker
                .tag_image(
                    &self.tag,
                    Some(bollard::query_parameters::TagImageOptions {
                        repo: Some(repo),
                        tag,
                    }),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to tag image: {}", e))?;
        }

        // Push if requested
        if self.push {
            println!();
            println!(
                "  {} Pushing {} to registry...",
                "📤".bright_yellow(),
                self.tag
            );

            Self::push_image(&docker, &self.tag, self.quiet).await?;

            // Push additional tags
            for additional_tag in &self.additional_tags {
                println!(
                    "  {} Pushing {} to registry...",
                    "📤".bright_yellow(),
                    additional_tag
                );
                Self::push_image(&docker, additional_tag, self.quiet).await?;
            }

            println!();
            println!("  {} Image pushed successfully", "✅".bright_green());
        }

        println!();
        println!("{}", "Build complete!".bright_green().bold());
        println!();
        println!("To deploy this image to Temps, run:");
        println!(
            "  {}",
            format!(
                "temps deploy image --image {} --project <project> --environment <env>",
                self.tag
            )
            .bright_cyan()
        );

        Ok(())
    }

    /// Detect the native platform for Docker builds
    /// Returns the platform string in the format "linux/arch"
    fn get_native_platform() -> String {
        #[cfg(target_arch = "x86_64")]
        {
            "linux/amd64".to_string()
        }
        #[cfg(target_arch = "aarch64")]
        {
            "linux/arm64".to_string()
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            // Fallback to amd64 for other architectures
            "linux/amd64".to_string()
        }
    }

    async fn create_tar_context(
        context_path: &PathBuf,
    ) -> anyhow::Result<http_body_util::Full<bytes::Bytes>> {
        use bytes::Bytes;
        use http_body_util::Full;

        let mut tar_buffer = Vec::new();
        {
            let mut tar_builder = tar::Builder::new(&mut tar_buffer);

            // Walk directory and add files, respecting .dockerignore
            let dockerignore_path = context_path.join(".dockerignore");
            let ignore_patterns = if dockerignore_path.exists() {
                Self::parse_dockerignore(&dockerignore_path)?
            } else {
                Vec::new()
            };

            Self::add_dir_to_tar(
                &mut tar_builder,
                context_path,
                context_path,
                &ignore_patterns,
            )?;

            tar_builder
                .finish()
                .map_err(|e| anyhow::anyhow!("Failed to create tar archive: {}", e))?;
        }

        Ok(Full::new(Bytes::from(tar_buffer)))
    }

    fn parse_dockerignore(path: &PathBuf) -> anyhow::Result<Vec<glob::Pattern>> {
        let content = std::fs::read_to_string(path)?;
        let mut patterns = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Convert dockerignore pattern to glob pattern
            let pattern_str = if line.starts_with('!') {
                // Negation patterns not fully supported, skip for now
                continue;
            } else if line.starts_with('/') {
                line[1..].to_string()
            } else {
                format!("**/{}", line)
            };

            if let Ok(pattern) = glob::Pattern::new(&pattern_str) {
                patterns.push(pattern);
            }
        }

        Ok(patterns)
    }

    fn add_dir_to_tar<W: std::io::Write>(
        builder: &mut tar::Builder<W>,
        base_path: &PathBuf,
        current_path: &PathBuf,
        ignore_patterns: &[glob::Pattern],
    ) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(current_path)? {
            let entry = entry?;
            let path = entry.path();
            let relative_path = path.strip_prefix(base_path)?.to_path_buf();
            let relative_str = relative_path.to_string_lossy();

            // Check if path matches any ignore pattern
            let should_ignore = ignore_patterns
                .iter()
                .any(|p| p.matches(&relative_str) || p.matches(&format!("{}/", relative_str)));

            // Always ignore .git directory
            if relative_str == ".git" || relative_str.starts_with(".git/") {
                continue;
            }

            if should_ignore {
                continue;
            }

            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                Self::add_dir_to_tar(builder, base_path, &path, ignore_patterns)?;
            } else if metadata.is_file() {
                builder
                    .append_path_with_name(&path, &relative_path)
                    .map_err(|e| anyhow::anyhow!("Failed to add file to tar: {}", e))?;
            }
        }

        Ok(())
    }

    fn parse_image_ref(image_ref: &str) -> anyhow::Result<(String, Option<String>)> {
        if let Some((repo, tag)) = image_ref.rsplit_once(':') {
            // Make sure we're not splitting on a port number
            // e.g., localhost:5000/myimage:latest should split as (localhost:5000/myimage, latest)
            if !tag.contains('/') {
                return Ok((repo.to_string(), Some(tag.to_string())));
            }
        }
        Ok((image_ref.to_string(), None))
    }

    async fn push_image(
        docker: &bollard::Docker,
        image_ref: &str,
        quiet: bool,
    ) -> anyhow::Result<()> {
        use futures::StreamExt;

        let (repo, tag) = Self::parse_image_ref(image_ref)?;

        let push_options = bollard::query_parameters::PushImageOptions {
            tag: Some(tag.unwrap_or_else(|| "latest".to_string())),
            platform: None, // Use default platform
        };

        // Use credentials from Docker config if available
        let mut push_stream = docker.push_image(&repo, Some(push_options), None);

        while let Some(result) = push_stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(error) = info.error {
                        return Err(anyhow::anyhow!("Push failed: {}", error));
                    }
                    if !quiet {
                        if let Some(status) = info.status {
                            if let Some(progress) = info.progress {
                                print!("\r  {} {} {}", "📤".bright_yellow(), status, progress);
                            } else {
                                println!("  {} {}", "📤".bright_yellow(), status);
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Push failed: {}", e));
                }
            }
        }

        if !quiet {
            println!();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_image_ref_with_tag() {
        let (repo, tag) = BuildCommand::parse_image_ref("myregistry.io/myapp:v1.0").unwrap();
        assert_eq!(repo, "myregistry.io/myapp");
        assert_eq!(tag, Some("v1.0".to_string()));
    }

    #[test]
    fn test_parse_image_ref_without_tag() {
        let (repo, tag) = BuildCommand::parse_image_ref("myregistry.io/myapp").unwrap();
        assert_eq!(repo, "myregistry.io/myapp");
        assert_eq!(tag, None);
    }

    #[test]
    fn test_parse_image_ref_with_port() {
        let (repo, tag) = BuildCommand::parse_image_ref("localhost:5000/myapp:latest").unwrap();
        assert_eq!(repo, "localhost:5000/myapp");
        assert_eq!(tag, Some("latest".to_string()));
    }

    #[test]
    fn test_parse_image_ref_with_port_no_tag() {
        let (repo, tag) = BuildCommand::parse_image_ref("localhost:5000/myapp").unwrap();
        assert_eq!(repo, "localhost:5000/myapp");
        assert_eq!(tag, None);
    }
}
