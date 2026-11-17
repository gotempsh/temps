// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Drop guards that clean up probe execs and helper containers on every
//! exit path, including cancellation.

use crate::BuilderError;
use bollard::{
    query_parameters::{RemoveContainerOptions, StopContainerOptions},
    Docker,
};
use std::sync::Arc;
use tracing::{error, warn};

/// Own a possibly running deployment probe across cancellation and error paths.
/// Docker cannot kill only an exec, so cleanup stops its immutable candidate ID.
pub(super) struct ProbeExecCleanup {
    docker: Arc<Docker>,
    container_id: String,
    armed: bool,
}

impl ProbeExecCleanup {
    pub(super) fn new(docker: Arc<Docker>, container_id: String) -> Self {
        Self {
            docker,
            container_id,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProbeExecCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let docker = self.docker.clone();
        let container_id = self.container_id.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!(
                container_id,
                "Probe cleanup could not start because the async runtime stopped"
            );
            return;
        };
        runtime.spawn(async move {
            // Bound both request duration and retries. No container restart is
            // ever issued, even if retention has already stopped this candidate.
            for attempt in 0..3 {
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    docker.stop_container(
                        &container_id,
                        Some(StopContainerOptions {
                            t: Some(0),
                            ..Default::default()
                        }),
                    ),
                )
                .await;
                if matches!(
                    result,
                    Ok(Ok(()))
                        | Ok(Err(bollard::errors::Error::DockerResponseServerError {
                            status_code: 304 | 404,
                            ..
                        }))
                ) {
                    return;
                }
                warn!(
                    container_id,
                    attempt = attempt + 1,
                    "Probe candidate stop could not be confirmed"
                );
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(250 * (1 << attempt)))
                        .await;
                }
            }
            error!(
                container_id,
                "Probe candidate cleanup exhausted its bounded stop retries"
            );
        });
    }
}

pub(super) struct DockerContainerCleanupGuard {
    docker: Arc<Docker>,
    pub(super) container_id: String,
    image_name: String,
    source_path: String,
    armed: bool,
}

impl DockerContainerCleanupGuard {
    pub(super) fn new(
        docker: Arc<Docker>,
        container_id: String,
        image_name: &str,
        source_path: &str,
    ) -> Self {
        Self {
            docker,
            container_id,
            image_name: image_name.to_string(),
            source_path: source_path.to_string(),
            armed: true,
        }
    }

    pub(super) async fn cleanup(&mut self) -> Result<(), String> {
        self.docker
            .remove_container(
                &self.container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|error| error.to_string())?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for DockerContainerCleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let docker = self.docker.clone();
        let container_id = self.container_id.clone();
        let image_name = self.image_name.clone();
        let source_path = self.source_path.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            error!(
                container_id,
                image_name,
                source_path,
                "Could not schedule cancellation cleanup for Docker extraction container: no Tokio runtime"
            );
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = docker
                .remove_container(
                    &container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                error!(
                    container_id,
                    image_name,
                    source_path,
                    reason = %error,
                    "Failed to remove Docker extraction container during cancellation cleanup"
                );
            }
        });
    }
}

pub(super) fn combine_extraction_and_cleanup(
    operation: Result<(), BuilderError>,
    cleanup: Result<(), String>,
    container_id: &str,
    image_name: &str,
    source_path: &str,
) -> Result<(), BuilderError> {
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Ok(()), Err(cleanup_error)) => Err(BuilderError::Other(format!(
            "Static extraction completed for image '{image_name}' path '{source_path}', but failed to remove temporary Docker container '{container_id}': {cleanup_error}"
        ))),
        (Err(operation_error), Err(cleanup_error)) => Err(BuilderError::Other(format!(
            "Static extraction failed for image '{image_name}' path '{source_path}': {operation_error}; additionally failed to remove temporary Docker container '{container_id}': {cleanup_error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_and_cleanup_errors_preserve_both_causes() {
        let result = combine_extraction_and_cleanup(
            Err(BuilderError::InvalidContext("unsafe archive".to_string())),
            Err("daemon unavailable".to_string()),
            "container-1",
            "image-1",
            "/app/dist",
        )
        .expect_err("operation and cleanup failures must be reported");
        let message = result.to_string();
        assert!(message.contains("unsafe archive"));
        assert!(message.contains("daemon unavailable"));
        assert!(message.contains("container-1"));
    }

    #[test]
    fn cleanup_failure_turns_successful_extraction_into_error() {
        let result = combine_extraction_and_cleanup(
            Ok(()),
            Err("permission denied".to_string()),
            "container-2",
            "image-2",
            "/public",
        );
        assert!(matches!(result, Err(BuilderError::Other(message)) if
            message.contains("permission denied") && message.contains("container-2")));
    }
}
