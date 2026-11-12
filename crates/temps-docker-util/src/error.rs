// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors from the Docker helpers.
//!
//! Messages name the container, image or network and the operation. They never
//! carry an exec's command or environment, which can hold credentials.

use bollard::errors::Error as DockerError;

/// Why a Docker helper failed.
#[derive(Debug, thiserror::Error)]
pub enum DockerUtilError {
    /// The daemon refused to create the exec.
    #[error("Failed to create exec in container '{container}': {source}")]
    ExecCreate {
        container: String,
        source: DockerError,
    },

    /// The daemon refused to start the exec.
    #[error("Failed to start exec '{exec_id}' in container '{container}': {source}")]
    ExecStart {
        container: String,
        exec_id: String,
        source: DockerError,
    },

    /// The exec's output stream failed under [`crate::StreamErrorPolicy::Fail`].
    #[error("Failed to read output of exec '{exec_id}' in container '{container}': {source}")]
    ExecStream {
        container: String,
        exec_id: String,
        source: DockerError,
    },

    /// The exec could not be inspected under [`crate::ExitCodePolicy::Required`].
    #[error("Failed to inspect exec '{exec_id}' in container '{container}': {source}")]
    ExecInspect {
        container: String,
        exec_id: String,
        source: DockerError,
    },

    /// The exec did not finish within [`crate::ExecCaptureOptions::timeout`].
    /// The command may still be running in the container.
    #[error("Exec '{exec_id}' in container '{container}' did not finish within {timeout_secs}s")]
    ExecTimedOut {
        container: String,
        exec_id: String,
        timeout_secs: u64,
    },

    /// The caller's cancellation token fired while output was being read. The
    /// command may still be running in the container.
    #[error("Exec '{exec_id}' in container '{container}' was cancelled")]
    ExecCancelled { container: String, exec_id: String },

    /// The daemon or the registry behind it reported an error during a pull.
    #[error("Failed to pull image '{reference}': {source}")]
    PullFailed {
        reference: String,
        source: DockerError,
    },

    /// A pull produced no progress for its idle limit.
    #[error(
        "Failed to pull image '{reference}': no progress from the Docker daemon for {idle_secs}s"
    )]
    PullStalled { reference: String, idle_secs: u64 },

    /// A pull kept reporting progress but ran past its overall limit.
    #[error("Failed to pull image '{reference}': pull did not finish within {total_secs}s")]
    PullTimedOut { reference: String, total_secs: u64 },

    /// A network could not be listed or created.
    #[error("Failed to {operation} while ensuring Docker network '{name}': {source}")]
    Network {
        name: String,
        operation: &'static str,
        source: DockerError,
    },

    /// The host-side source of an upload could not be opened or read.
    #[error("Failed to read upload source '{path}': {reason}")]
    UploadSource { path: String, reason: String },

    /// The destination file name of an upload cannot be encoded in a tar header.
    #[error("Invalid upload file name '{name}': {reason}")]
    UploadInvalidName { name: String, reason: String },

    /// The Docker daemon stopped accepting data for longer than the stall
    /// timeout, before the whole archive was sent.
    #[error(
        "Upload of '{dest_name}' into container {container_id} stalled: the Docker daemon \
         accepted no data for {stall_secs}s after {sent} of {size} bytes"
    )]
    UploadStalled {
        container_id: String,
        dest_name: String,
        size: u64,
        sent: u64,
        stall_secs: u64,
    },

    /// The whole archive was sent, but the Docker daemon did not confirm the
    /// upload within the window allowed for writing out a file this size.
    #[error(
        "Upload of '{dest_name}' into container {container_id} was not confirmed: the Docker \
         daemon received all {size} bytes but did not answer within {waited_secs}s"
    )]
    UploadUnconfirmed {
        container_id: String,
        dest_name: String,
        size: u64,
        waited_secs: u64,
    },

    /// The daemon could not be reached or queried while checking whether an
    /// image is still in use, so it cannot be proved unused and is not
    /// removed.
    #[error("Failed to {operation} while checking which images are still in use: {source}")]
    ImageInventory {
        operation: &'static str,
        source: DockerError,
    },

    /// More images were passed to one removal call than it accepts.
    #[error("Refusing to remove {requested} images in one call; at most {max} are accepted")]
    ImageBatchTooLarge { requested: usize, max: usize },

    /// The Docker daemon rejected or aborted an upload.
    #[error(
        "Failed to upload '{dest_name}' ({size} bytes) into {container_id}:{dest_dir}: {reason}"
    )]
    Upload {
        container_id: String,
        dest_dir: String,
        dest_name: String,
        size: u64,
        reason: String,
    },
}

impl DockerUtilError {
    /// The daemon's error behind this one, or the error back if there is none
    /// (a timeout, a cancellation, a bad upload source). For a caller with an
    /// error type that carries a bollard error.
    pub fn into_docker_source(self) -> Result<DockerError, Self> {
        match self {
            Self::ExecCreate { source, .. }
            | Self::ExecStart { source, .. }
            | Self::ExecStream { source, .. }
            | Self::ExecInspect { source, .. }
            | Self::PullFailed { source, .. }
            | Self::Network { source, .. }
            | Self::ImageInventory { source, .. } => Ok(source),
            other @ (Self::ExecTimedOut { .. }
            | Self::ImageBatchTooLarge { .. }
            | Self::ExecCancelled { .. }
            | Self::PullStalled { .. }
            | Self::PullTimedOut { .. }
            | Self::UploadSource { .. }
            | Self::UploadInvalidName { .. }
            | Self::UploadStalled { .. }
            | Self::UploadUnconfirmed { .. }
            | Self::Upload { .. }) => Err(other),
        }
    }

    /// What went wrong, without the container, image or network this error
    /// names. For a caller whose own message already says what it was doing.
    pub fn cause_text(&self) -> String {
        match self {
            Self::ExecCreate { source, .. }
            | Self::ExecStart { source, .. }
            | Self::ExecStream { source, .. }
            | Self::ExecInspect { source, .. }
            | Self::PullFailed { source, .. }
            | Self::Network { source, .. }
            | Self::ImageInventory { source, .. } => source.to_string(),
            Self::PullStalled { idle_secs, .. } => {
                format!("no progress from the Docker daemon for {idle_secs}s")
            }
            Self::PullTimedOut { total_secs, .. } => {
                format!("pull did not finish within {total_secs}s")
            }
            Self::ExecTimedOut { .. }
            | Self::ExecCancelled { .. }
            | Self::ImageBatchTooLarge { .. }
            | Self::UploadSource { .. }
            | Self::UploadInvalidName { .. }
            | Self::UploadStalled { .. }
            | Self::UploadUnconfirmed { .. }
            | Self::Upload { .. } => self.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon_error() -> DockerError {
        DockerError::DockerResponseServerError {
            status_code: 500,
            message: "daemon said no".to_string(),
        }
    }

    #[test]
    fn messages_name_the_resource_and_the_operation() {
        let error = DockerUtilError::ExecCreate {
            container: "db-1".to_string(),
            source: daemon_error(),
        };
        let message = error.to_string();
        assert!(message.contains("create exec"), "{message}");
        assert!(message.contains("db-1"), "{message}");
        assert!(message.contains("daemon said no"), "{message}");

        let error = DockerUtilError::Network {
            name: "app-net".to_string(),
            operation: "list networks",
            source: daemon_error(),
        };
        assert!(error.to_string().contains("app-net"), "{error}");
        assert!(error.to_string().contains("list networks"), "{error}");
    }

    #[test]
    fn cause_text_is_the_daemons_message_alone() {
        let error = DockerUtilError::PullFailed {
            reference: "busybox:1".to_string(),
            source: daemon_error(),
        };
        assert_eq!(error.cause_text(), daemon_error().to_string());
    }

    #[test]
    fn pull_limits_say_what_ran_out() {
        let stalled = DockerUtilError::PullStalled {
            reference: "busybox:1".to_string(),
            idle_secs: 120,
        };
        assert_eq!(
            stalled.cause_text(),
            "no progress from the Docker daemon for 120s"
        );
        assert!(stalled.to_string().contains("busybox:1"));

        let timed_out = DockerUtilError::PullTimedOut {
            reference: "busybox:1".to_string(),
            total_secs: 900,
        };
        assert_eq!(timed_out.cause_text(), "pull did not finish within 900s");
    }

    #[test]
    fn only_daemon_failures_have_a_docker_source() {
        let failed = DockerUtilError::ExecInspect {
            container: "db-1".to_string(),
            exec_id: "abc".to_string(),
            source: daemon_error(),
        };
        assert!(failed.into_docker_source().is_ok());

        let cancelled = DockerUtilError::ExecCancelled {
            container: "db-1".to_string(),
            exec_id: "abc".to_string(),
        };
        assert!(matches!(
            cancelled.into_docker_source(),
            Err(DockerUtilError::ExecCancelled { .. })
        ));
    }
}
