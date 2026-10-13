// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Getting a dump file out of a one-shot backup container without a host
//! bind mount.
//!
//! ## Why not a bind mount
//!
//! The logical-dump engines used to bind-mount a directory under
//! `std::env::temp_dir()` into the sidecar and read the dump back from the
//! same host path. That only works when the Docker daemon sees the host's
//! filesystem. When Docker runs in a VM (Colima, Docker Desktop, Lima) or on
//! a remote host, the bind source is created *inside the VM*: the sidecar
//! writes its dump there, exits 0, and the Temps host finds nothing at the
//! path. The leftover file then also breaks the next retry (`gzip: File
//! exists`), because the host cannot delete a file that lives in the VM.
//!
//! ## What this module does instead
//!
//! - The sidecar writes into its **own** filesystem, under an
//!   attempt-scoped directory ([`DumpAttempt::container_dir`]).
//! - The container is created without `auto_remove`
//!   ([`run_retained_one_shot`]), so its filesystem survives the command.
//! - The dump is streamed out through the Docker archive API
//!   (`GET /containers/{id}/archive`), un-tarred on the fly and written to an
//!   attempt-scoped host file. Memory stays constant: chunks flow through a
//!   bounded channel into a blocking tar reader that copies straight to disk
//!   ([`copy_file_out_of_container`], shared with the provider crate).
//! - The container is removed on every path; the host working directory is a
//!   [`tempfile::TempDir`] owned by exactly one attempt and deleted on drop.
//!
//! Every attempt gets a fresh random id, so two attempts of the same backup
//! never share a container name, a container path or a host path, and a file
//! left behind by an earlier attempt (or by an older binary that used the
//! bind mount) cannot collide with a new one.

use std::path::{Path, PathBuf};

use bollard::Docker;
use temps_backup_core::engine_v2::BackupError;
use temps_providers::externalsvc::container_download::read_file_tail;
pub use temps_providers::externalsvc::container_download::{
    copy_file_out_of_container, CopyOutFailure,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::oneshot::{run_retained_one_shot, OneShotError, OneShotSpec, RetainedOneShot};

/// Root directory, inside the sidecar's own filesystem, under which each
/// attempt writes its output. Never bind-mounted.
const CONTAINER_WORK_ROOT: &str = "/tmp/temps-backup";
/// Upper bound on the diagnostics carried in an error message.
const MAX_DIAGNOSTIC_BYTES: usize = 4_000;

/// Failure of a logical dump that runs in a one-shot sidecar.
///
/// The variants separate *where* the failure happened, because each has a
/// different fix: the database tool failing ([`Self::Export`]), the dump not
/// being readable by Temps after the tool succeeded ([`Self::DumpUnreadable`]),
/// and the object-storage upload failing ([`Self::Upload`]).
#[derive(Debug, thiserror::Error)]
pub enum DumpCaptureError {
    /// The backup was cancelled while the sidecar ran or the dump was being
    /// copied out.
    #[error("{tool} backup cancelled")]
    Cancelled { tool: &'static str },

    /// The attempt-scoped host working directory could not be created.
    #[error(
        "Could not create a working directory for {tool} backup {backup_uuid} under \
         '{parent}' on the Temps host: {reason}"
    )]
    HostWorkdir {
        tool: &'static str,
        backup_uuid: String,
        parent: String,
        reason: String,
    },

    /// Docker could not create, start or wait on the sidecar.
    #[error("{tool} backup container '{container}' did not run to completion: {source}")]
    Container {
        tool: &'static str,
        container: String,
        /// Boxed: `OneShotError` carries a bollard error and would make every
        /// `Result<_, DumpCaptureError>` large.
        #[source]
        source: Box<OneShotError>,
    },

    /// A dump that runs through `docker exec` in the service's own container
    /// (rather than in a one-shot sidecar) could not be run or streamed out:
    /// the exec could not be created, started or inspected, the output stream
    /// broke, or writing it to the attempt's host file failed.
    #[error("{tool} dump could not be streamed out of container '{container}' through docker exec: {reason}")]
    Exec {
        tool: &'static str,
        container: String,
        reason: String,
        /// Mirrors [`BackupError::is_permanent`] of the underlying failure,
        /// so wrapping the error never changes the executor's retry policy.
        permanent: bool,
    },

    /// The dump tool itself failed (bad credentials, unreachable target,
    /// tool error). Retrying usually needs a configuration change.
    #[error("{tool} export failed in backup container '{container}' with exit code {exit_code}: {stderr}")]
    Export {
        tool: &'static str,
        container: String,
        exit_code: i64,
        stderr: String,
    },

    /// The dump tool succeeded, but Temps could not read the file it wrote.
    #[error(
        "{tool} exported the backup, but Temps could not read the temporary file written by \
         Docker. Copying '{container_path}' out of backup container '{container}' through the \
         Docker API into '{host_path}' failed: {reason}. Check that the Docker daemon is \
         reachable and that the Temps host has free space in its temporary directory, then retry."
    )]
    DumpUnreadable {
        tool: &'static str,
        container: String,
        container_path: String,
        host_path: String,
        reason: String,
    },

    /// The dump tool succeeded and the file was read, but it is empty.
    #[error(
        "{tool} exited successfully but wrote an empty dump to '{container_path}' in backup \
         container '{container}'"
    )]
    EmptyDump {
        tool: &'static str,
        container: String,
        container_path: String,
    },

    /// The dump was captured, but uploading it to object storage failed.
    #[error(
        "{tool} backup was captured, but uploading it to s3://{bucket}/{key} failed: {reason}"
    )]
    Upload {
        tool: &'static str,
        bucket: String,
        key: String,
        reason: String,
        /// Mirrors [`BackupError::is_permanent`] of the underlying failure,
        /// so wrapping the error never changes the executor's retry policy.
        permanent: bool,
    },
}

impl DumpCaptureError {
    /// Classify a failure from [`super::v2_common::upload_file`] as an
    /// upload failure, preserving cancellation and permanence.
    pub fn upload(tool: &'static str, bucket: &str, key: &str, error: BackupError) -> Self {
        let permanent = error.is_permanent();
        let reason = match error {
            BackupError::Cancelled => return Self::Cancelled { tool },
            BackupError::Failed { reason }
            | BackupError::PermanentFailure { reason }
            | BackupError::Timeout { reason } => reason,
        };
        Self::Upload {
            tool,
            bucket: bucket.to_string(),
            key: key.to_string(),
            reason,
            permanent,
        }
    }

    /// Classify a failure from [`super::mariadb_exec::exec_stream_stdout_to_file`]
    /// as an exec-stream failure, preserving cancellation and permanence.
    pub fn exec(tool: &'static str, container: &str, error: BackupError) -> Self {
        let permanent = error.is_permanent();
        let reason = match error {
            BackupError::Cancelled => return Self::Cancelled { tool },
            BackupError::Failed { reason }
            | BackupError::PermanentFailure { reason }
            | BackupError::Timeout { reason } => reason,
        };
        Self::Exec {
            tool,
            container: container.to_string(),
            reason,
            permanent,
        }
    }
}

impl From<DumpCaptureError> for BackupError {
    fn from(error: DumpCaptureError) -> Self {
        match error {
            DumpCaptureError::Cancelled { .. } => BackupError::Cancelled,
            DumpCaptureError::Upload {
                permanent: true, ..
            }
            | DumpCaptureError::Exec {
                permanent: true, ..
            } => BackupError::PermanentFailure {
                reason: error.to_string(),
            },
            DumpCaptureError::Upload {
                permanent: false, ..
            }
            | DumpCaptureError::Exec {
                permanent: false, ..
            }
            | DumpCaptureError::HostWorkdir { .. }
            | DumpCaptureError::Container { .. }
            | DumpCaptureError::Export { .. }
            | DumpCaptureError::DumpUnreadable { .. }
            | DumpCaptureError::EmptyDump { .. } => BackupError::Failed {
                reason: error.to_string(),
            },
        }
    }
}

/// One attempt of one backup: a random attempt id plus a host working
/// directory that belongs to this attempt alone.
///
/// Dropping the attempt deletes its host directory and everything in it,
/// and nothing else: files from earlier attempts, other backups or older
/// binaries are never touched.
#[derive(Debug)]
pub struct DumpAttempt {
    backup_uuid: String,
    attempt_id: String,
    host_dir: tempfile::TempDir,
}

impl DumpAttempt {
    /// Start an attempt with its host directory under the OS temp dir. The
    /// directory is only ever written by Temps itself (never bind-mounted),
    /// so it does not need to be visible to Docker.
    pub fn new(
        tool: &'static str,
        engine: &str,
        backup_uuid: &str,
    ) -> Result<Self, DumpCaptureError> {
        Self::new_in(&std::env::temp_dir(), tool, engine, backup_uuid)
    }

    /// Start an attempt with its host directory under `parent`.
    pub fn new_in(
        parent: &Path,
        tool: &'static str,
        engine: &str,
        backup_uuid: &str,
    ) -> Result<Self, DumpCaptureError> {
        let attempt_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let host_dir = tempfile::Builder::new()
            .prefix(&format!(
                "temps-{engine}-backup-{backup_uuid}-{attempt_id}-"
            ))
            .tempdir_in(parent)
            .map_err(|e| DumpCaptureError::HostWorkdir {
                tool,
                backup_uuid: backup_uuid.to_string(),
                parent: parent.display().to_string(),
                reason: e.to_string(),
            })?;
        Ok(Self {
            backup_uuid: backup_uuid.to_string(),
            attempt_id,
            host_dir,
        })
    }

    /// Random id of this attempt.
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    /// Container name for this attempt: unique per attempt, so a container
    /// left over from a crashed attempt can never block the next one with a
    /// "name already in use" conflict.
    pub fn container_name(&self, prefix: &str) -> String {
        format!("{prefix}-{}-{}", self.backup_uuid, self.attempt_id)
    }

    /// Directory inside the sidecar's own filesystem for this attempt.
    pub fn container_dir(&self) -> String {
        format!("{CONTAINER_WORK_ROOT}/{}", self.attempt_id)
    }

    /// Path of `file_name` inside [`Self::container_dir`].
    pub fn container_path(&self, file_name: &str) -> String {
        format!("{}/{file_name}", self.container_dir())
    }

    /// This attempt's host working directory.
    pub fn host_dir(&self) -> &Path {
        self.host_dir.path()
    }

    /// Path of `file_name` inside [`Self::host_dir`].
    pub fn host_path(&self, file_name: &str) -> PathBuf {
        self.host_dir.path().join(file_name)
    }
}

/// What [`capture_dump`] should run and read back.
#[derive(Debug)]
pub struct CaptureRequest {
    /// Human name of the dump tool's database, used in user-facing errors
    /// (`"Redis"`, `"MongoDB"`, `"PostgreSQL"`).
    pub tool: &'static str,
    /// The sidecar. It must write its dump to `container_path`; it must not
    /// rely on bind mounts for its output.
    pub spec: OneShotSpec,
    /// Absolute path of the dump inside the sidecar.
    pub container_path: String,
    /// Host path the dump is copied to. Must not exist yet.
    pub host_path: PathBuf,
    /// Optional `(container path, host path)` of a small diagnostics file the
    /// command writes (for example a redirected stderr). Copied out only when
    /// the command fails, and appended to the [`DumpCaptureError::Export`]
    /// message.
    pub failure_log: Option<(String, PathBuf)>,
}

/// Run the sidecar, then stream its dump out through the Docker archive API.
///
/// Returns the size of the host copy in bytes. The sidecar container is
/// removed on every path, including cancellation.
pub async fn capture_dump(
    docker: &Docker,
    request: CaptureRequest,
    cancel: &CancellationToken,
) -> Result<u64, DumpCaptureError> {
    let CaptureRequest {
        tool,
        spec,
        container_path,
        host_path,
        failure_log,
    } = request;
    let container_name = spec.name.clone();

    let RetainedOneShot { result, container } =
        match run_retained_one_shot(docker, spec, cancel).await {
            Ok(run) => run,
            Err(OneShotError::Cancelled) => return Err(DumpCaptureError::Cancelled { tool }),
            Err(source) => {
                return Err(DumpCaptureError::Container {
                    tool,
                    container: container_name,
                    source: Box::new(source),
                })
            }
        };

    let outcome = async {
        if result.exit_code != 0 {
            let mut stderr = bounded_tail(&result.stderr_tail, MAX_DIAGNOSTIC_BYTES);
            if let Some((log_container_path, log_host_path)) = failure_log.as_ref() {
                if let Some(log) = read_failure_log(
                    docker,
                    container.name(),
                    log_container_path,
                    log_host_path,
                    cancel,
                )
                .await
                {
                    stderr = if stderr.is_empty() {
                        log
                    } else {
                        format!("{log} (container stderr: {stderr})")
                    };
                }
            }
            return Err(DumpCaptureError::Export {
                tool,
                container: container_name.clone(),
                exit_code: result.exit_code,
                stderr,
            });
        }

        let size = copy_file_out_of_container(
            docker,
            container.name(),
            &container_path,
            &host_path,
            cancel,
        )
        .await
        .map_err(|failure| match failure {
            CopyOutFailure::Cancelled => DumpCaptureError::Cancelled { tool },
            CopyOutFailure::Failed(reason) => DumpCaptureError::DumpUnreadable {
                tool,
                container: container_name.clone(),
                container_path: container_path.clone(),
                host_path: host_path.display().to_string(),
                reason,
            },
        })?;
        if size == 0 {
            return Err(DumpCaptureError::EmptyDump {
                tool,
                container: container_name.clone(),
                container_path: container_path.clone(),
            });
        }
        Ok(size)
    }
    .await;

    container.remove().await;
    outcome
}

/// Copy the diagnostics file of a failed command, returning its bounded
/// tail. Best effort: a missing file just means there is nothing to add.
async fn read_failure_log(
    docker: &Docker,
    container: &str,
    container_path: &str,
    host_path: &Path,
    cancel: &CancellationToken,
) -> Option<String> {
    match copy_file_out_of_container(docker, container, container_path, host_path, cancel).await {
        Ok(_) => match read_file_tail(host_path, MAX_DIAGNOSTIC_BYTES).await {
            Ok(bytes) => Some(bounded_tail(
                &String::from_utf8_lossy(&bytes),
                MAX_DIAGNOSTIC_BYTES,
            ))
            .filter(|log| !log.is_empty()),
            Err(e) => {
                debug!(path = %host_path.display(), error = %e, "dump_capture: could not read copied failure log");
                None
            }
        },
        Err(failure) => {
            debug!(
                container,
                container_path,
                ?failure,
                "dump_capture: no failure log to copy"
            );
            None
        }
    }
}

/// Trim `value` and keep at most its last `max_bytes` bytes, on a UTF-8
/// boundary.
pub fn bounded_tail(value: &str, max_bytes: usize) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - max_bytes;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Attempt scoping ──────────────────────────────────────────────────

    #[test]
    fn two_attempts_of_one_backup_never_share_names_or_paths() {
        let parent = tempfile::tempdir().expect("parent dir");
        let first = DumpAttempt::new_in(parent.path(), "Redis", "redis", "b-uuid").expect("first");
        let second =
            DumpAttempt::new_in(parent.path(), "Redis", "redis", "b-uuid").expect("second");

        assert_ne!(first.attempt_id(), second.attempt_id());
        assert_ne!(
            first.container_name("temps-redis-backup"),
            second.container_name("temps-redis-backup")
        );
        assert_ne!(first.container_dir(), second.container_dir());
        assert_ne!(first.host_dir(), second.host_dir());
        assert_ne!(
            first.host_path("dump.rdb.gz"),
            second.host_path("dump.rdb.gz")
        );

        let name = first.container_name("temps-redis-backup");
        assert!(name.starts_with("temps-redis-backup-b-uuid-"), "{name}");
        assert!(first
            .container_path("dump.rdb.gz")
            .starts_with("/tmp/temps-backup/"));
        assert!(first.host_dir().starts_with(parent.path()));
    }

    #[test]
    fn leftovers_from_earlier_attempts_neither_block_nor_get_deleted() {
        let parent = tempfile::tempdir().expect("parent dir");
        // The layout an older binary left behind on a failed bind-mount run.
        let legacy = parent.path().join("b-uuid.rdb.gz");
        std::fs::write(&legacy, b"stale").expect("legacy leftover");
        // An earlier attempt of this binary that still holds its file.
        let earlier =
            DumpAttempt::new_in(parent.path(), "Redis", "redis", "b-uuid").expect("earlier");
        std::fs::write(earlier.host_path("dump.rdb.gz"), b"earlier").expect("earlier dump");

        let retry = DumpAttempt::new_in(parent.path(), "Redis", "redis", "b-uuid").expect("retry");
        let retry_dump = retry.host_path("dump.rdb.gz");
        assert!(
            !retry_dump.exists(),
            "a new attempt starts from a clean path"
        );
        std::fs::write(&retry_dump, b"retry").expect("retry dump");
        let retry_dir = retry.host_dir().to_path_buf();
        drop(retry);

        assert!(!retry_dir.exists(), "the attempt removes its own directory");
        assert_eq!(std::fs::read(&legacy).expect("legacy kept"), b"stale");
        assert_eq!(
            std::fs::read(earlier.host_path("dump.rdb.gz")).expect("earlier kept"),
            b"earlier"
        );
    }

    #[test]
    fn attempt_reports_an_unusable_parent_as_a_host_workdir_error() {
        let parent = tempfile::tempdir().expect("parent dir");
        let not_a_dir = parent.path().join("file");
        std::fs::write(&not_a_dir, b"x").expect("file");

        let error = DumpAttempt::new_in(&not_a_dir, "Redis", "redis", "b-uuid")
            .expect_err("a file cannot hold the attempt directory");
        assert!(matches!(
            error,
            DumpCaptureError::HostWorkdir { ref backup_uuid, .. } if backup_uuid == "b-uuid"
        ));
        assert!(error.to_string().contains(&not_a_dir.display().to_string()));
    }

    // ── Error classification ─────────────────────────────────────────────

    #[test]
    fn unreadable_dump_is_reported_as_a_readback_failure_not_a_redis_failure() {
        let error = DumpCaptureError::DumpUnreadable {
            tool: "Redis",
            container: "temps-redis-backup-b-a".into(),
            container_path: "/tmp/temps-backup/a/dump.rdb.gz".into(),
            host_path: "/host/tmp/dump.rdb.gz".into(),
            reason: "the Docker archive download failed: 404".into(),
        };
        let message = error.to_string();
        assert!(message.starts_with(
            "Redis exported the backup, but Temps could not read the temporary file written by Docker."
        ));
        for detail in [
            "temps-redis-backup-b-a",
            "/tmp/temps-backup/a/dump.rdb.gz",
            "/host/tmp/dump.rdb.gz",
            "404",
        ] {
            assert!(message.contains(detail), "missing {detail}: {message}");
        }
        assert!(matches!(
            BackupError::from(error),
            BackupError::Failed { reason } if reason.starts_with("Redis exported the backup")
        ));
    }

    #[test]
    fn export_failure_names_the_tool_container_and_exit_code() {
        let error = DumpCaptureError::Export {
            tool: "Redis",
            container: "temps-redis-backup-b-a".into(),
            exit_code: 1,
            stderr: "Could not connect to Redis at redis-x:6379".into(),
        };
        let message = error.to_string();
        assert!(message.starts_with("Redis export failed"), "{message}");
        assert!(message.contains("exit code 1"));
        assert!(message.contains("Could not connect"));
        assert!(!message.contains("could not read the temporary file"));
        assert!(matches!(
            BackupError::from(error),
            BackupError::Failed { .. }
        ));
    }

    #[test]
    fn upload_failures_keep_their_retry_policy_and_are_labelled_as_uploads() {
        let transient = DumpCaptureError::upload(
            "Redis",
            "bucket",
            "key.rdb.gz",
            BackupError::Failed {
                reason: "503 Slow Down".into(),
            },
        );
        assert!(matches!(
            transient,
            DumpCaptureError::Upload {
                permanent: false,
                ..
            }
        ));
        let message = transient.to_string();
        assert!(message.contains("uploading it to s3://bucket/key.rdb.gz failed: 503 Slow Down"));
        assert!(matches!(
            BackupError::from(transient),
            BackupError::Failed { .. }
        ));

        let permanent = DumpCaptureError::upload(
            "Redis",
            "bucket",
            "key",
            BackupError::PermanentFailure {
                reason: "AccessDenied".into(),
            },
        );
        assert!(matches!(
            BackupError::from(permanent),
            BackupError::PermanentFailure { reason } if reason.contains("AccessDenied")
        ));

        let cancelled = DumpCaptureError::upload("Redis", "bucket", "key", BackupError::Cancelled);
        assert!(matches!(cancelled, DumpCaptureError::Cancelled { .. }));
        assert!(matches!(
            BackupError::from(cancelled),
            BackupError::Cancelled
        ));
    }

    #[tokio::test]
    async fn failure_log_tail_is_read_without_loading_the_whole_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("failure.log");
        let mut log = "x".repeat(MAX_DIAGNOSTIC_BYTES * 50);
        log.push_str("the real error");
        std::fs::write(&path, &log).expect("write log");

        let tail = read_file_tail(&path, MAX_DIAGNOSTIC_BYTES)
            .await
            .expect("read tail");
        assert!(tail.len() <= MAX_DIAGNOSTIC_BYTES + 4, "{}", tail.len());
        assert!(String::from_utf8_lossy(&tail).ends_with("the real error"));

        std::fs::write(&path, "short log").expect("write short log");
        let short = read_file_tail(&path, MAX_DIAGNOSTIC_BYTES)
            .await
            .expect("read short");
        assert_eq!(short, b"short log");
    }

    #[test]
    fn bounded_tail_keeps_the_end_on_a_char_boundary() {
        assert_eq!(bounded_tail("  short  ", 100), "short");
        let tail = bounded_tail(&format!("{}END", "é".repeat(100)), 11);
        assert!(tail.starts_with('…'));
        assert!(tail.ends_with("END"));
    }
}
