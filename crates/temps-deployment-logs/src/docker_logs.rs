// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Docker container logging service
//!
//! This module provides utilities for:
//! - Retrieving container logs efficiently
//! - Following container logs in real-time
//! - Checking container status
//! - Saving container logs to files

use std::sync::Arc;

use bollard::{query_parameters::LogsOptions, Docker};
use futures::{StreamExt, TryStreamExt};
use temps_core::{DockerHandle, UtcDateTime};

#[derive(Debug, Clone)]
pub struct DockerLogService {
    docker: Arc<DockerHandle>,
}

#[derive(Debug, thiserror::Error)]
pub enum DockerLogError {
    #[error("Docker API error: {0}")]
    DockerError(#[from] bollard::errors::Error),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Container not found: {container_id}")]
    ContainerNotFound { container_id: String },
    /// The local Docker daemon is not available in this serve profile.
    /// Container log streaming requires a Docker daemon on the same host.
    #[error(transparent)]
    DockerUnavailable(#[from] temps_core::DockerUnavailable),
}
// Common/shared types
#[derive(Debug)]
pub struct ContainerLogOptions {
    pub start_date: Option<UtcDateTime>,
    pub end_date: Option<UtcDateTime>,
    pub tail: Option<String>, // "all" or number of lines
    pub timestamps: bool,     // Include timestamps in log output
    pub follow: bool,         // Follow log output (like `docker logs -f`)
}
impl DockerLogService {
    /// Create a new `DockerLogService` that will call `docker.require()` at
    /// each method invocation. Registered unconditionally — callers receive
    /// a typed [`DockerLogError::DockerUnavailable`] on a control-plane
    /// process instead of a missing-service panic.
    pub fn new(docker: Arc<DockerHandle>) -> Self {
        DockerLogService { docker }
    }

    pub async fn get_container_logs(
        &self,
        container_id: &str,
        options: ContainerLogOptions,
    ) -> Result<impl futures::Stream<Item = Result<String, DockerLogError>>, DockerLogError> {
        let docker: Arc<Docker> = self.docker.require()?;
        // Validate container exists first by attempting to inspect it
        docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await?;

        let tail = options.tail.unwrap_or_else(|| "all".to_string());
        let log_options = Some(LogsOptions {
            follow: options.follow,
            stdout: true,
            stderr: true,
            timestamps: options.timestamps,
            tail,
            since: options
                .start_date
                .map(|dt| dt.timestamp() as i32)
                .unwrap_or(0),
            until: options
                .end_date
                .map(|dt| dt.timestamp() as i32)
                .unwrap_or(0),
        });

        let logs = docker.logs(container_id, log_options);

        let stream = logs.map(|result| match result {
            Ok(log_output) => Ok(String::from_utf8_lossy(&log_output.into_bytes()).to_string()),
            Err(e) => Err(DockerLogError::DockerError(e)),
        });

        Ok(stream)
    }

    pub async fn follow_container_logs(
        &self,
        container_id: &str,
        timestamps: bool,
    ) -> Result<impl futures::Stream<Item = Result<String, DockerLogError>>, DockerLogError> {
        let docker: Arc<Docker> = self.docker.require()?;
        let options = LogsOptions {
            stdout: true,
            stderr: true,
            follow: true,
            timestamps,
            ..Default::default()
        };

        let logs_stream = docker
            .logs(container_id, Some(options))
            .map(|chunk| match chunk {
                Ok(c) => Ok(String::from_utf8_lossy(&c.into_bytes()).to_string()),
                Err(e) => Err(DockerLogError::DockerError(e)),
            });

        Ok(logs_stream)
    }

    pub async fn get_logs_with_timestamps(
        &self,
        container_id: &str,
        lines: Option<i32>,
    ) -> Result<String, DockerLogError> {
        let docker: Arc<Docker> = self.docker.require()?;
        let mut options = LogsOptions {
            stdout: true,
            stderr: true,
            timestamps: true,
            ..Default::default()
        };

        if let Some(tail) = lines {
            options.tail = tail.to_string();
        }

        let logs_stream = docker
            .logs(container_id, Some(options))
            .map(|chunk| chunk.map(|c| String::from_utf8_lossy(&c.into_bytes()).to_string()))
            .try_collect::<Vec<_>>()
            .await?;

        Ok(logs_stream.join(""))
    }

    pub async fn save_logs_to_file(
        &self,
        container_id: &str,
        file_path: &str,
        lines: Option<i32>,
    ) -> Result<(), DockerLogError> {
        let _options = ContainerLogOptions {
            start_date: None,
            end_date: None,
            tail: lines.map(|l| l.to_string()),
            timestamps: true,
            follow: false,
        };
        let logs = self.get_logs_with_timestamps(container_id, lines).await?;
        tokio::fs::write(file_path, logs).await?;
        Ok(())
    }

    pub async fn is_container_running(&self, container_id: &str) -> Result<bool, DockerLogError> {
        let docker: Arc<Docker> = self.docker.require()?;
        match docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(container) => {
                if let Some(state) = container.state {
                    Ok(state.running.unwrap_or(false))
                } else {
                    Ok(false)
                }
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Err(DockerLogError::ContainerNotFound {
                container_id: container_id.to_string(),
            }),
            Err(e) => Err(DockerLogError::DockerError(e)),
        }
    }

    pub async fn get_container_name(&self, container_id: &str) -> Result<String, DockerLogError> {
        let docker: Arc<Docker> = self.docker.require()?;
        let container = docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await?;

        Ok(container
            .name
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use temps_core::{DockerHandle, PROFILE_CONTROL_PLANE};

    /// Helper: build a DockerLogService backed by the local daemon, or skip.
    /// Returns `None` when no daemon is reachable so tests degrade gracefully.
    fn make_service_if_available() -> Option<DockerLogService> {
        match Docker::connect_with_local_defaults() {
            Ok(docker) => Some(DockerLogService::new(Arc::new(DockerHandle::available(
                Arc::new(docker),
            )))),
            Err(_) => None,
        }
    }

    #[tokio::test]
    async fn test_docker_log_service_creation() {
        // DockerLogService can always be constructed (holds DockerHandle, not Docker).
        let handle = DockerHandle::disabled(PROFILE_CONTROL_PLANE, "unit test");
        let _svc = DockerLogService::new(Arc::new(handle));
    }

    #[tokio::test]
    async fn test_docker_unavailable_returns_typed_error() {
        let handle = DockerHandle::disabled(PROFILE_CONTROL_PLANE, "unit test");
        let svc = DockerLogService::new(Arc::new(handle));
        let err = svc
            .is_container_running("any-container")
            .await
            .expect_err("disabled handle must produce an error");
        assert!(
            matches!(err, DockerLogError::DockerUnavailable(_)),
            "expected DockerUnavailable, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_docker_log_service_error_types() {
        // Test error type creation
        let error = DockerLogError::ContainerNotFound {
            container_id: "test-container".to_string(),
        };
        assert!(error.to_string().contains("Container not found"));
        assert!(error.to_string().contains("test-container"));
    }

    #[tokio::test]
    async fn test_docker_container_running_check_invalid_container() {
        let Some(docker_service) = make_service_if_available() else {
            println!("Docker not available, skipping");
            return;
        };

        // This should return an error for a non-existent container
        let result = docker_service
            .is_container_running("non-existent-container-12345")
            .await;

        // Should return ContainerNotFound error
        match result {
            Err(DockerLogError::ContainerNotFound { .. }) => {
                // Expected behavior
            }
            Err(_) => {
                // Other Docker errors are also acceptable (e.g., Docker not running)
            }
            Ok(false) => {
                // Some Docker versions might return false instead of error
            }
            Ok(true) => {
                panic!("Non-existent container should not be running");
            }
        }
    }

    #[tokio::test]
    async fn test_docker_get_logs_invalid_container() {
        let Some(docker_service) = make_service_if_available() else {
            println!("Docker not available, skipping");
            return;
        };

        // This should fail for a non-existent container
        let options = ContainerLogOptions {
            start_date: None,
            end_date: None,
            tail: Some("10".to_string()),
            timestamps: true,
            follow: false,
        };
        let result = docker_service
            .get_container_logs("non-existent-container-12345", options)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_docker_get_logs_with_timestamps_invalid_container() {
        let Some(docker_service) = make_service_if_available() else {
            println!("Docker not available, skipping");
            return;
        };

        // This should fail for a non-existent container
        let result = docker_service
            .get_logs_with_timestamps("non-existent-container-12345", Some(5))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_docker_save_logs_to_file_invalid_container() {
        let Some(docker_service) = make_service_if_available() else {
            println!("Docker not available, skipping");
            return;
        };
        let temp_dir = TempDir::new().unwrap();
        let log_file = temp_dir.path().join("docker-logs.txt");

        // This should fail for a non-existent container
        let result = docker_service
            .save_logs_to_file(
                "non-existent-container-12345",
                log_file.to_str().unwrap(),
                Some(10),
            )
            .await;
        assert!(result.is_err());
    }
}
