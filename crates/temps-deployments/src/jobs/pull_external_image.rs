//! Pull External Image Job
//!
//! Pulls and verifies an external Docker image from a registry.
//! This job is used for remote deployments where images are built externally
//! and only the pre-built image reference is provided.

use async_trait::async_trait;
use bollard::image::CreateImageOptions;
use bollard::Docker;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_logs::{LogLevel, LogService};
use tracing::{debug, error, info};

/// Output from PullExternalImageJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullExternalImageOutput {
    /// The image reference that was pulled
    pub image_ref: String,
    /// Image ID (sha256:...)
    pub image_id: String,
    /// Image size in bytes
    pub size_bytes: u64,
    /// Image tag
    pub tag: String,
    /// Image digest if available (sha256:...)
    pub digest: Option<String>,
}

impl PullExternalImageOutput {
    /// Extract PullExternalImageOutput from WorkflowContext
    pub fn from_context(context: &WorkflowContext, job_id: &str) -> Result<Self, WorkflowError> {
        let image_ref: String = context.get_output(job_id, "image_ref")?.ok_or_else(|| {
            WorkflowError::JobValidationFailed("image_ref output not found".to_string())
        })?;
        let image_id: String = context.get_output(job_id, "image_id")?.ok_or_else(|| {
            WorkflowError::JobValidationFailed("image_id output not found".to_string())
        })?;
        let size_bytes: u64 = context.get_output(job_id, "size_bytes")?.ok_or_else(|| {
            WorkflowError::JobValidationFailed("size_bytes output not found".to_string())
        })?;
        let tag: String = context.get_output(job_id, "tag")?.ok_or_else(|| {
            WorkflowError::JobValidationFailed("tag output not found".to_string())
        })?;
        let digest: Option<String> = context.get_output(job_id, "digest")?;

        Ok(Self {
            image_ref,
            image_id,
            size_bytes,
            tag,
            digest,
        })
    }
}

/// Job that pulls an external Docker image from a registry
pub struct PullExternalImageJob {
    /// Unique job identifier
    job_id: String,
    /// External image reference (e.g., "ghcr.io/org/app:v1.0")
    image_ref: String,
    /// Optional external image ID (from external_images table)
    external_image_id: Option<i32>,
    /// Docker client
    docker: Arc<Docker>,
    /// Log service for streaming logs
    log_service: Option<Arc<LogService>>,
    /// Log ID for this job's logs
    log_id: Option<String>,
}

impl std::fmt::Debug for PullExternalImageJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PullExternalImageJob")
            .field("job_id", &self.job_id)
            .field("image_ref", &self.image_ref)
            .field("external_image_id", &self.external_image_id)
            .finish()
    }
}

impl PullExternalImageJob {
    pub fn new(
        job_id: String,
        image_ref: String,
        external_image_id: Option<i32>,
        docker: Arc<Docker>,
    ) -> Self {
        Self {
            job_id,
            image_ref,
            external_image_id,
            docker,
            log_service: None,
            log_id: None,
        }
    }

    pub fn with_log_service(mut self, log_service: Arc<LogService>, log_id: String) -> Self {
        self.log_service = Some(log_service);
        self.log_id = Some(log_id);
        self
    }

    /// Parse image reference to extract registry, image name, and tag
    fn parse_image_ref(&self) -> (Option<String>, String, String) {
        let image_ref = &self.image_ref;

        // Extract tag (after last ':')
        let (image_name, tag) = if let Some(idx) = image_ref.rfind(':') {
            // Check if ':' is part of a port number in registry (e.g., "localhost:5000/app")
            let potential_tag = &image_ref[idx + 1..];
            if potential_tag.contains('/') {
                // It's a port number, not a tag
                (image_ref.as_str(), "latest")
            } else {
                (&image_ref[..idx], potential_tag)
            }
        } else {
            (image_ref.as_str(), "latest")
        };

        // Extract registry (before first '/')
        let parts: Vec<&str> = image_name.split('/').collect();
        let registry = if parts.len() > 1 && (parts[0].contains('.') || parts[0].contains(':')) {
            Some(parts[0].to_string())
        } else {
            None // Default to Docker Hub
        };

        (registry, image_name.to_string(), tag.to_string())
    }

    async fn log(&self, level: LogLevel, message: &str) {
        if let (Some(log_service), Some(log_id)) = (&self.log_service, &self.log_id) {
            if let Err(e) = log_service
                .append_structured_log(log_id, level, message)
                .await
            {
                error!("Failed to write to log: {}", e);
            }
        }
    }
}

#[async_trait]
impl WorkflowTask for PullExternalImageJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Pull External Image"
    }

    fn description(&self) -> &str {
        "Pulls and verifies an external Docker image from a registry"
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        let (registry, _image_name, tag) = self.parse_image_ref();

        info!(
            "Pulling external image: {} (registry: {:?}, tag: {})",
            self.image_ref,
            registry.as_deref().unwrap_or("docker.io"),
            tag
        );

        self.log(
            LogLevel::Info,
            &format!("🐳 Pulling external image: {}", self.image_ref),
        )
        .await;

        if let Some(external_image_id) = self.external_image_id {
            self.log(
                LogLevel::Info,
                &format!("📦 External image ID: {}", external_image_id),
            )
            .await;
        }

        // Pull the image using Docker daemon
        let create_image_options = CreateImageOptions {
            from_image: self.image_ref.clone(),
            ..Default::default()
        };

        // Use None for authentication - relies on Docker daemon's credentials (~/.docker/config.json)
        let mut stream = self
            .docker
            .create_image(Some(create_image_options), None, None);

        let mut pull_succeeded = false;
        let mut last_error: Option<String> = None;

        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    // Log progress
                    if let Some(status) = &info.status {
                        let progress = info
                            .progress
                            .as_ref()
                            .map(|p| format!(" {}", p))
                            .unwrap_or_default();

                        debug!("Pull progress: {}{}", status, progress);

                        // Log key events
                        if status.contains("Pulling") || status.contains("Downloading") {
                            self.log(LogLevel::Info, &format!("⏳ {}{}", status, progress))
                                .await;
                        } else if status.contains("Pull complete")
                            || status.contains("Download complete")
                        {
                            self.log(LogLevel::Success, &format!("✅ {}", status)).await;
                        } else if status.contains("Already exists") {
                            self.log(LogLevel::Info, &format!("📦 {}", status)).await;
                        }
                    }

                    // Check for digest in final status
                    if let Some(status) = &info.status {
                        if status.contains("Digest:") || status.contains("Status:") {
                            pull_succeeded = true;
                        }
                    }
                }
                Err(e) => {
                    let error_msg = format!("Pull error: {}", e);
                    error!("{}", error_msg);
                    self.log(LogLevel::Error, &format!("❌ {}", error_msg))
                        .await;
                    last_error = Some(error_msg);
                }
            }
        }

        if !pull_succeeded {
            if let Some(error) = last_error {
                return Ok(JobResult::failure(
                    context,
                    format!("Failed to pull image {}: {}", self.image_ref, error),
                ));
            }
            // Check if we can still find the image (might have been pulled previously)
        }

        // Inspect the image to get details
        self.log(LogLevel::Info, "🔍 Inspecting pulled image...")
            .await;

        let image_inspect = self
            .docker
            .inspect_image(&self.image_ref)
            .await
            .map_err(|e| {
                WorkflowError::JobExecutionFailed(format!(
                    "Failed to inspect image {}: {}",
                    self.image_ref, e
                ))
            })?;

        let image_id = image_inspect.id.unwrap_or_default();
        let size_bytes = image_inspect.size.unwrap_or(0) as u64;

        // Extract digest from repo digests
        let digest = image_inspect
            .repo_digests
            .as_ref()
            .and_then(|digests| digests.first())
            .and_then(|d| d.rsplit('@').next())
            .map(String::from);

        self.log(
            LogLevel::Success,
            &format!(
                "✅ Image pulled successfully: {} ({:.2} MB)",
                self.image_ref,
                size_bytes as f64 / 1024.0 / 1024.0
            ),
        )
        .await;

        // Store outputs in context
        context.set_output(&self.job_id, "image_ref", &self.image_ref)?;
        context.set_output(&self.job_id, "image_id", &image_id)?;
        context.set_output(&self.job_id, "size_bytes", size_bytes)?;
        context.set_output(&self.job_id, "tag", &tag)?;
        context.set_output(&self.job_id, "digest", &digest)?;
        // Also store as image_tag for compatibility with DeployImageJob
        context.set_output(&self.job_id, "image_tag", &self.image_ref)?;

        Ok(JobResult::success_with_message(
            context,
            format!(
                "Successfully pulled image: {} ({})",
                self.image_ref, image_id
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_image_ref_with_registry_and_tag() {
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let job = PullExternalImageJob::new(
            "test".to_string(),
            "ghcr.io/org/app:v1.0".to_string(),
            None,
            docker,
        );

        let (registry, image_name, tag) = job.parse_image_ref();
        assert_eq!(registry, Some("ghcr.io".to_string()));
        assert_eq!(image_name, "ghcr.io/org/app");
        assert_eq!(tag, "v1.0");
    }

    #[test]
    fn test_parse_image_ref_docker_hub() {
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let job =
            PullExternalImageJob::new("test".to_string(), "nginx:latest".to_string(), None, docker);

        let (registry, image_name, tag) = job.parse_image_ref();
        assert_eq!(registry, None);
        assert_eq!(image_name, "nginx");
        assert_eq!(tag, "latest");
    }

    #[test]
    fn test_parse_image_ref_with_port() {
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let job = PullExternalImageJob::new(
            "test".to_string(),
            "localhost:5000/myapp:v2".to_string(),
            None,
            docker,
        );

        let (registry, image_name, tag) = job.parse_image_ref();
        assert_eq!(registry, Some("localhost:5000".to_string()));
        assert_eq!(image_name, "localhost:5000/myapp");
        assert_eq!(tag, "v2");
    }

    #[test]
    fn test_parse_image_ref_no_tag() {
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let job = PullExternalImageJob::new(
            "test".to_string(),
            "myregistry.io/app".to_string(),
            None,
            docker,
        );

        let (registry, image_name, tag) = job.parse_image_ref();
        assert_eq!(registry, Some("myregistry.io".to_string()));
        assert_eq!(image_name, "myregistry.io/app");
        assert_eq!(tag, "latest");
    }
}
