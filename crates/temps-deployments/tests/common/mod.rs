//! Common test utilities shared across nixpacks integration tests
//!
//! This module provides reusable test infrastructure including:
//! - Mock git providers for local fixtures
//! - Capturing log writers for test verification
//! - Docker cleanup utilities
//! - Test assertions helpers

use async_trait::async_trait;
use bollard::Docker;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::{LogWriter, WorkflowCancellationProvider, WorkflowError};
use temps_git::{GitProviderManagerError, GitProviderManagerTrait, RepositoryInfo};
use tokio::sync::Mutex;

/// No-op cancellation provider for tests (never cancels)
pub struct NoCancellationProvider;

#[async_trait]
impl WorkflowCancellationProvider for NoCancellationProvider {
    async fn is_cancelled(&self, _workflow_run_id: &str) -> Result<bool, WorkflowError> {
        Ok(false)
    }
}

/// Capturing LogWriter that stores all log messages for test verification
pub struct CapturingLogWriter {
    stage_id: i32,
    logs: Arc<Mutex<Vec<String>>>,
}

impl CapturingLogWriter {
    pub fn new(stage_id: i32) -> Self {
        Self {
            stage_id,
            logs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn get_logs(&self) -> Vec<String> {
        self.logs.lock().await.clone()
    }
}

#[async_trait]
impl LogWriter for CapturingLogWriter {
    async fn write_log(&self, message: String) -> Result<(), WorkflowError> {
        self.logs.lock().await.push(message);
        Ok(())
    }

    fn stage_id(&self) -> i32 {
        self.stage_id
    }
}

/// Mock GitProviderManager that copies from local fixture directory
/// instead of actually cloning from a remote repository
pub struct LocalFixtureGitProvider {
    pub fixture_path: PathBuf,
}

#[async_trait]
impl GitProviderManagerTrait for LocalFixtureGitProvider {
    async fn clone_repository(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
        target_dir: &std::path::Path,
        _branch_or_ref: Option<&str>,
    ) -> Result<(), GitProviderManagerError> {
        // Directory should not exist or should be empty
        if target_dir.exists() {
            let is_empty = std::fs::read_dir(target_dir)
                .map_err(|e| {
                    GitProviderManagerError::CloneError(format!("Failed to read directory: {}", e))
                })?
                .next()
                .is_none();

            if !is_empty {
                return Err(GitProviderManagerError::DirectoryNotEmpty(
                    target_dir.display().to_string(),
                ));
            }
        }

        // Create directory and copy fixture files
        std::fs::create_dir_all(target_dir).map_err(|e| {
            GitProviderManagerError::CloneError(format!("Failed to create directory: {}", e))
        })?;

        copy_dir_recursive(&self.fixture_path, target_dir).map_err(|e| {
            GitProviderManagerError::CloneError(format!("Failed to copy fixture: {}", e))
        })?;

        Ok(())
    }

    async fn get_repository_info(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
    ) -> Result<RepositoryInfo, GitProviderManagerError> {
        Ok(RepositoryInfo {
            clone_url: "file://fixture".to_string(),
            default_branch: "main".to_string(),
            owner: "test".to_string(),
            name: "fixture-app".to_string(),
        })
    }

    async fn download_archive(
        &self,
        _connection_id: i32,
        _repo_owner: &str,
        _repo_name: &str,
        _branch_or_ref: &str,
        _archive_path: &std::path::Path,
    ) -> Result<(), GitProviderManagerError> {
        // Force fallback to clone for fixtures
        Err(GitProviderManagerError::Other(
            "Archive not available for fixtures".to_string(),
        ))
    }
}

/// Recursively copy directory contents (dst must already exist)
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

/// Cleanup Docker test resources (containers and images)
pub async fn cleanup_test_resources(docker: &Docker, container_id: &str, image_tag: &str) {
    use bollard::container::{RemoveContainerOptions, StopContainerOptions};
    use bollard::image::RemoveImageOptions;

    println!("🧹 Cleaning up test resources...");

    // Stop and remove container
    let _ = docker
        .stop_container(container_id, None::<StopContainerOptions>)
        .await;

    let _ = docker
        .remove_container(
            container_id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    // Remove image
    let _ = docker
        .remove_image(
            image_tag,
            Some(RemoveImageOptions {
                force: true,
                ..Default::default()
            }),
            None,
        )
        .await;

    println!("  ✓ Cleanup completed");
}

/// Check if Docker is available and running
pub async fn check_docker_available() -> Option<Arc<Docker>> {
    match Docker::connect_with_local_defaults() {
        Ok(docker) => {
            let docker = Arc::new(docker);
            if docker.ping().await.is_ok() {
                Some(docker)
            } else {
                println!("⚠️  Docker daemon not responding");
                None
            }
        }
        Err(_) => {
            println!("⚠️  Docker not available");
            None
        }
    }
}

/// Verify that a container is running and healthy
pub async fn verify_container_running(docker: &Docker, container_id: &str) -> bool {
    use bollard::container::InspectContainerOptions;

    match docker
        .inspect_container(container_id, None::<InspectContainerOptions>)
        .await
    {
        Ok(inspect) => {
            let is_running = inspect
                .state
                .as_ref()
                .and_then(|s| s.running)
                .unwrap_or(false);

            if is_running {
                println!("  ✓ Container {} is running", &container_id[..12]);
            } else {
                println!("  ✗ Container {} is not running", &container_id[..12]);
            }

            is_running
        }
        Err(e) => {
            println!("  ✗ Failed to inspect container: {}", e);
            false
        }
    }
}

/// Print container logs for debugging
pub async fn print_container_logs(docker: &Docker, container_id: &str, tail: &str) {
    use bollard::container::LogsOptions;
    use futures_util::stream::StreamExt;

    println!("📄 Container logs (last {} lines):", tail);

    let log_options = Some(LogsOptions::<String> {
        stdout: true,
        stderr: true,
        tail: tail.to_string(),
        ..Default::default()
    });

    let mut log_stream = docker.logs(container_id, log_options);
    let mut log_count = 0;

    while let Some(log_result) = log_stream.next().await {
        if let Ok(log_output) = log_result {
            print!("  {}", log_output);
            log_count += 1;
        }
    }

    if log_count == 0 {
        println!("  (no logs available)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_capturing_log_writer() {
        let writer = CapturingLogWriter::new(1);

        writer.write_log("Test log 1".to_string()).await.unwrap();
        writer.write_log("Test log 2".to_string()).await.unwrap();

        let logs = writer.get_logs().await;
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0], "Test log 1");
        assert_eq!(logs[1], "Test log 2");
    }

    #[tokio::test]
    async fn test_no_cancellation_provider() {
        let provider = NoCancellationProvider;
        let result = provider.is_cancelled("test-workflow").await;

        assert!(result.is_ok());
        assert!(!result.unwrap());
    }
}
