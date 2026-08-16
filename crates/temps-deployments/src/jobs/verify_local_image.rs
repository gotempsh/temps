//! Verify Local Image Job
//!
//! Verifies that a locally uploaded Docker image exists and is accessible.
//! This job is used for deployments where images are uploaded directly via
//! `docker save` / `docker load` rather than pulled from a registry.

use async_trait::async_trait;
use bollard::Docker;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{JobResult, WorkflowContext, WorkflowError, WorkflowTask};
use temps_logs::{LogLevel, LogService};
use tracing::{error, info};

/// Output from VerifyLocalImageJob
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyLocalImageOutput {
    /// The image reference that was verified
    pub image_ref: String,
    /// Image ID (sha256:...)
    pub image_id: String,
    /// Image size in bytes
    pub size_bytes: u64,
    /// Image tag
    pub tag: String,
}

impl VerifyLocalImageOutput {
    /// Extract VerifyLocalImageOutput from WorkflowContext
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

        Ok(Self {
            image_ref,
            image_id,
            size_bytes,
            tag,
        })
    }
}

/// Job that verifies a locally uploaded Docker image exists
pub struct VerifyLocalImageJob {
    /// Unique job identifier
    job_id: String,
    /// Image reference to verify (e.g., "temps-myapp-production:upload-12345")
    image_ref: String,
    /// Expected image ID (optional, for verification)
    expected_image_id: Option<String>,
    /// Docker client
    docker: Arc<Docker>,
    /// Log service for streaming logs
    log_service: Option<Arc<LogService>>,
    /// Log ID for this job's logs
    log_id: Option<String>,
}

impl std::fmt::Debug for VerifyLocalImageJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyLocalImageJob")
            .field("job_id", &self.job_id)
            .field("image_ref", &self.image_ref)
            .field("expected_image_id", &self.expected_image_id)
            .finish()
    }
}

fn normalize_image_id(id: &str) -> String {
    id.strip_prefix("sha256:").unwrap_or(id).to_lowercase()
}

fn image_ids_match(actual_id: &str, expected_id: &str) -> bool {
    let actual_normalized = normalize_image_id(actual_id);
    let expected_normalized = normalize_image_id(expected_id);

    if actual_normalized.is_empty() || expected_normalized.is_empty() {
        return false;
    }
    if expected_normalized.len() == 12
        && expected_normalized
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        actual_normalized.starts_with(&expected_normalized)
    } else {
        actual_normalized == expected_normalized
    }
}

impl VerifyLocalImageJob {
    pub fn new(
        job_id: String,
        image_ref: String,
        expected_image_id: Option<String>,
        docker: Arc<Docker>,
    ) -> Self {
        Self {
            job_id,
            image_ref,
            expected_image_id,
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

    /// Extract tag from image reference
    fn extract_tag(&self) -> String {
        if let Some(idx) = self.image_ref.rfind(':') {
            let potential_tag = &self.image_ref[idx + 1..];
            // Check if ':' is part of a port number (e.g., "localhost:5000/app")
            if !potential_tag.contains('/') {
                return potential_tag.to_string();
            }
        }
        "latest".to_string()
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
impl WorkflowTask for VerifyLocalImageJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Verify Local Image"
    }

    fn description(&self) -> &str {
        "Verifies that a locally uploaded Docker image exists"
    }

    async fn execute(&self, mut context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        let tag = self.extract_tag();

        info!(
            "Verifying locally uploaded image: {} (expected_id: {:?})",
            self.image_ref,
            self.expected_image_id
                .as_deref()
                .map(|s| &s[..12.min(s.len())])
        );

        self.log(
            LogLevel::Info,
            &format!("🔍 Verifying locally uploaded image: {}", self.image_ref),
        )
        .await;

        // Inspect the image to verify it exists
        let image_inspect = match self.docker.inspect_image(&self.image_ref).await {
            Ok(inspect) => inspect,
            Err(e) => {
                let error_msg = format!(
                    "Failed to find uploaded image '{}'. The image may not have been loaded correctly: {}",
                    self.image_ref, e
                );
                error!("{}", error_msg);
                self.log(LogLevel::Error, &format!("❌ {}", error_msg))
                    .await;
                return Ok(JobResult::failure(context, error_msg));
            }
        };

        let image_id = image_inspect.id.clone().unwrap_or_default();
        let size_bytes = image_inspect.size.unwrap_or(0) as u64;

        // Verify the image ID still matches the image that was imported for this deployment.
        // Docker tags are mutable and shared across the daemon, so a mismatch means deploying
        // by this tag could run a different image with this deployment's configuration.
        if let Some(ref expected_id) = self.expected_image_id {
            if !image_ids_match(&image_id, expected_id) {
                let error_msg = format!(
                    "Image ID mismatch for local image '{}': expected '{}', found '{}'. Refusing to deploy mutable tag.",
                    self.image_ref, expected_id, image_id
                );
                error!("{}", error_msg);
                self.log(LogLevel::Error, &format!("❌ {}", error_msg))
                    .await;
                return Ok(JobResult::failure(context, error_msg));
            }
        }

        self.log(
            LogLevel::Success,
            &format!(
                "✅ Image verified: {} ({:.2} MB)",
                self.image_ref,
                size_bytes as f64 / 1024.0 / 1024.0
            ),
        )
        .await;

        // Log image details
        if let Some(config) = &image_inspect.config {
            if let Some(exposed_ports) = &config.exposed_ports {
                if !exposed_ports.is_empty() {
                    self.log(
                        LogLevel::Info,
                        &format!("📡 Exposed ports: {}", exposed_ports.join(", ")),
                    )
                    .await;
                }
            }
        }

        // Store outputs in context (compatible with PullExternalImageJob output)
        context.set_output(&self.job_id, "image_ref", &self.image_ref)?;
        context.set_output(&self.job_id, "image_id", &image_id)?;
        context.set_output(&self.job_id, "size_bytes", size_bytes)?;
        context.set_output(&self.job_id, "tag", &tag)?;
        // Also store as image_tag for compatibility with DeployImageJob
        context.set_output(&self.job_id, "image_tag", &self.image_ref)?;

        Ok(JobResult::success_with_message(
            context,
            format!(
                "Successfully verified local image: {} ({})",
                self.image_ref, image_id
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_ids_match_full_ids() {
        assert!(image_ids_match(
            "sha256:abcdef1234567890",
            "sha256:abcdef1234567890"
        ));
    }

    #[test]
    fn test_image_ids_match_expected_short_id() {
        assert!(image_ids_match("sha256:abcdef1234567890", "abcdef123456"));
    }

    #[test]
    fn test_image_ids_match_detects_mismatch() {
        assert!(!image_ids_match("sha256:abcdef1234567890", "123456abcdef"));
        assert!(!image_ids_match("sha256:abcdef1234567890", "a"));
        assert!(!image_ids_match("sha256:abcdef1234567890", "not-hex-id!!"));
        assert!(!image_ids_match(
            "sha256:abcdef1234567890",
            "sha256:abcdef1234560000"
        ));
        assert!(!image_ids_match("", ""));
    }

    #[test]
    fn test_extract_tag_with_tag() {
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let job = VerifyLocalImageJob::new(
            "test".to_string(),
            "temps-myapp:upload-12345".to_string(),
            None,
            docker,
        );

        assert_eq!(job.extract_tag(), "upload-12345");
    }

    #[test]
    fn test_extract_tag_no_tag() {
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let job =
            VerifyLocalImageJob::new("test".to_string(), "temps-myapp".to_string(), None, docker);

        assert_eq!(job.extract_tag(), "latest");
    }

    #[test]
    fn test_extract_tag_with_port_in_registry() {
        let docker = Arc::new(
            bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker"),
        );
        let job = VerifyLocalImageJob::new(
            "test".to_string(),
            "localhost:5000/myapp:v1.0".to_string(),
            None,
            docker,
        );

        assert_eq!(job.extract_tag(), "v1.0");
    }

    #[tokio::test]
    async fn immutable_image_id_mismatch_fails_closed() {
        use bollard::query_parameters::CreateImageOptionsBuilder;
        use futures::StreamExt;

        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }

        docker
            .create_image(
                Some(
                    CreateImageOptionsBuilder::new()
                        .from_image("hello-world:latest")
                        .build(),
                ),
                None,
                None,
            )
            .for_each(|_| async {})
            .await;
        if docker.inspect_image("hello-world:latest").await.is_err() {
            println!("Could not fetch a test image (offline?), skipping");
            return;
        }

        let job = VerifyLocalImageJob::new(
            "verify-immutable".to_string(),
            "hello-world:latest".to_string(),
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000000".into()),
            Arc::new(docker),
        );
        let context = crate::test_utils::create_test_context("run-mismatch".into(), 1, 1, 1);
        let result = job.execute(context).await.expect("verification result");

        assert_eq!(result.status, temps_core::JobStatus::Failure);
        assert!(result
            .message
            .as_deref()
            .is_some_and(|message| message.contains("Image ID mismatch")
                && message.contains("Refusing to deploy mutable tag")));
    }
}
