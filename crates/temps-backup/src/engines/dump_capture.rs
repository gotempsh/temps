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
//!   bounded channel into a blocking tar reader that copies straight to disk.
//! - The container is removed on every path; the host working directory is a
//!   [`tempfile::TempDir`] owned by exactly one attempt and deleted on drop.
//!
//! Every attempt gets a fresh random id, so two attempts of the same backup
//! never share a container name, a container path or a host path, and a file
//! left behind by an earlier attempt (or by an older binary that used the
//! bind mount) cannot collide with a new one.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use bollard::query_parameters::DownloadFromContainerOptionsBuilder;
use bollard::Docker;
use bytes::{Buf, Bytes};
use futures::StreamExt;
use temps_backup_core::engine_v2::BackupError;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::oneshot::{run_retained_one_shot, OneShotError, OneShotSpec, RetainedOneShot};

/// Root directory, inside the sidecar's own filesystem, under which each
/// attempt writes its output. Never bind-mounted.
const CONTAINER_WORK_ROOT: &str = "/tmp/temps-backup";
/// Upper bound on the diagnostics carried in an error message.
const MAX_DIAGNOSTIC_BYTES: usize = 4_000;
/// Chunks buffered between the Docker download and the tar extractor.
/// Bollard yields chunks of at most a few tens of KiB, so this bounds the
/// in-flight memory to well under a MiB regardless of dump size.
const DOWNLOAD_CHANNEL_CAPACITY: usize = 8;

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
}

impl From<DumpCaptureError> for BackupError {
    fn from(error: DumpCaptureError) -> Self {
        match error {
            DumpCaptureError::Cancelled { .. } => BackupError::Cancelled,
            DumpCaptureError::Upload {
                permanent: true, ..
            } => BackupError::PermanentFailure {
                reason: error.to_string(),
            },
            DumpCaptureError::Upload {
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

/// Read at most the last `max_bytes` (plus a few bytes of slack so a UTF-8
/// character cut by the seek can be dropped cleanly) of the file at `path`.
/// A failing tool can write an arbitrarily large diagnostics file; only its
/// tail is ever shown, so only its tail is loaded into memory.
async fn read_file_tail(path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let window = (max_bytes as u64).saturating_add(4);
    if len > window {
        file.seek(std::io::SeekFrom::Start(len - window)).await?;
    }
    let mut tail = Vec::with_capacity(len.min(window) as usize);
    file.take(window).read_to_end(&mut tail).await?;
    Ok(tail)
}

/// Why [`copy_file_out_of_container`] did not produce the file.
#[derive(Debug, PartialEq, Eq)]
pub enum CopyOutFailure {
    Cancelled,
    Failed(String),
}

/// Stream one regular file out of a container (running or exited) into
/// `host_path`, which must not exist yet. Returns the bytes written.
///
/// The Docker archive API returns a tar stream; it is fed through a bounded
/// channel into a blocking tar reader that copies the single entry straight
/// to disk, so memory use does not depend on the file size. A partial host
/// file is deleted on failure.
pub async fn copy_file_out_of_container(
    docker: &Docker,
    container: &str,
    container_path: &str,
    host_path: &Path,
    cancel: &CancellationToken,
) -> Result<u64, CopyOutFailure> {
    let expected_name = Path::new(container_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            CopyOutFailure::Failed(format!(
                "'{container_path}' does not name a file inside the container"
            ))
        })?
        .to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(DOWNLOAD_CHANNEL_CAPACITY);
    let dest = host_path.to_path_buf();
    let extractor = tokio::task::spawn_blocking(move || {
        extract_single_file(ChannelReader::new(rx), &expected_name, &dest)
    });

    let mut stream = docker.download_from_container(
        container,
        Some(
            DownloadFromContainerOptionsBuilder::new()
                .path(container_path)
                .build(),
        ),
    );
    let mut download_error: Option<String> = None;
    let mut cancelled = false;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
                break;
            }
            next = stream.next() => match next {
                Some(Ok(chunk)) => {
                    // The extractor stops reading once it has the file (or
                    // has failed); its error is reported below.
                    if tx.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                }
                Some(Err(e)) => {
                    let message = e.to_string();
                    let _ = tx.send(Err(std::io::Error::other(message.clone()))).await;
                    download_error = Some(message);
                    break;
                }
                None => break,
            }
        }
    }
    // Closing the channel ends the extractor's input, so it always returns.
    drop(tx);
    drop(stream);
    let (extracted, created) = match extractor.await {
        Ok(Extracted { result, created }) => (result, created),
        Err(join_error) => (
            Err(format!("the tar extraction task failed: {join_error}")),
            false,
        ),
    };

    let failure = if cancelled {
        CopyOutFailure::Cancelled
    } else if let Some(message) = download_error {
        CopyOutFailure::Failed(format!("the Docker archive download failed: {message}"))
    } else {
        match extracted {
            Ok(size) => return Ok(size),
            Err(reason) => CopyOutFailure::Failed(reason),
        }
    };
    // Only a file this call created is deleted: `create_new` refuses to open
    // a path that already exists, so a pre-existing file is never touched.
    if created {
        remove_if_present(host_path).await;
    }
    Err(failure)
}

/// Result of [`extract_single_file`], plus whether it created `dest` (and
/// so owns any partial file left there on failure).
struct Extracted {
    result: Result<u64, String>,
    created: bool,
}

async fn remove_if_present(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(path = %path.display(), error = %e, "dump_capture: could not delete partial copy");
        }
    }
}

/// Read the single regular file `expected_name` from a tar stream into a new
/// file at `dest`. Fails, rather than returning a short count, when the
/// stream ends before the entry's declared size.
fn extract_single_file<R: Read>(reader: R, expected_name: &str, dest: &Path) -> Extracted {
    let mut created = false;
    let result = extract_single_file_inner(reader, expected_name, dest, &mut created);
    Extracted { result, created }
}

fn extract_single_file_inner<R: Read>(
    reader: R,
    expected_name: &str,
    dest: &Path,
    created: &mut bool,
) -> Result<u64, String> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| format!("could not read the tar archive returned by Docker: {e}"))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| format!("could not read the tar archive returned by Docker: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let entry_path = entry
            .path()
            .map_err(|e| format!("the tar archive returned by Docker has an unreadable path: {e}"))?
            .into_owned();
        let entry_name = entry_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if entry_name != expected_name {
            return Err(format!(
                "the tar archive returned by Docker contained '{}' instead of '{expected_name}'",
                entry_path.display()
            ));
        }
        let declared = entry
            .header()
            .size()
            .map_err(|e| format!("the tar entry for '{expected_name}' has an invalid size: {e}"))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .map_err(|e| {
                format!(
                    "could not create '{}' on the Temps host: {e}",
                    dest.display()
                )
            })?;
        *created = true;
        let copied = std::io::copy(&mut entry, &mut file).map_err(|e| {
            format!(
                "copying '{expected_name}' into '{}' failed: {e}",
                dest.display()
            )
        })?;
        if copied != declared {
            return Err(format!(
                "the tar stream for '{expected_name}' ended after {copied} of {declared} bytes"
            ));
        }
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("could not flush '{}' to disk: {e}", dest.display()))?;
        return Ok(copied);
    }
    Err(format!(
        "the tar archive returned by Docker contained no regular file named '{expected_name}'"
    ))
}

/// Blocking `Read` over a bounded channel of chunks: the bridge between the
/// async Docker download and the synchronous `tar` reader. Ends (EOF) when
/// the sender is dropped; an `Err` chunk surfaces as a read error.
struct ChannelReader {
    rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
    current: Bytes,
}

impl ChannelReader {
    fn new(rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>) -> Self {
        Self {
            rx,
            current: Bytes::new(),
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.current.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => self.current = chunk,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.current.len());
        buf[..n].copy_from_slice(&self.current[..n]);
        self.current.advance(n);
        Ok(n)
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
    use std::io::Cursor;

    fn tar_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, name, *data)
                .expect("append tar entry");
        }
        builder.into_inner().expect("finish tar")
    }

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

    // ── Tar extraction ───────────────────────────────────────────────────

    #[test]
    fn extracts_the_single_expected_file() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let archive = tar_with(&[("dump.rdb.gz", b"payload-bytes")]);

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        assert_eq!(extracted.result, Ok(13));
        assert!(extracted.created);
        assert_eq!(std::fs::read(&dest).expect("dest"), b"payload-bytes");
    }

    #[test]
    fn rejects_an_archive_holding_a_different_file() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let archive = tar_with(&[("other.bin", b"x")]);

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        let reason = extracted.result.expect_err("wrong entry");
        assert!(reason.contains("other.bin"), "{reason}");
        assert!(!extracted.created);
        assert!(!dest.exists());
    }

    #[test]
    fn rejects_an_archive_without_a_regular_file() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "dump.rdb.gz/", std::io::empty())
            .expect("dir entry");
        let archive = builder.into_inner().expect("finish");

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        assert!(extracted
            .result
            .expect_err("no file")
            .contains("no regular file named 'dump.rdb.gz'"));
    }

    #[test]
    fn a_truncated_stream_fails_instead_of_returning_a_short_file() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let payload = vec![7_u8; 4096];
        let mut archive = tar_with(&[("dump.rdb.gz", payload.as_slice())]);
        // Header (512) plus half the payload: the stream ends mid-entry.
        archive.truncate(512 + 2048);

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        assert!(extracted.result.is_err(), "{:?}", extracted.result);
        assert!(extracted.created, "the partial file is ours to delete");
    }

    #[test]
    fn never_overwrites_or_claims_a_pre_existing_destination() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        std::fs::write(&dest, b"not ours").expect("pre-existing");
        let archive = tar_with(&[("dump.rdb.gz", b"new")]);

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        assert!(extracted.result.is_err());
        assert!(
            !extracted.created,
            "a file this call did not create must not be cleaned up by it"
        );
        assert_eq!(std::fs::read(&dest).expect("kept"), b"not ours");
    }

    // ── Channel bridge ───────────────────────────────────────────────────

    #[tokio::test]
    async fn channel_reader_streams_chunks_into_the_tar_reader() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let payload: Vec<u8> = (0..200_000_u32).map(|i| (i % 251) as u8).collect();
        let archive = tar_with(&[("dump.rdb.gz", payload.as_slice())]);

        let (tx, rx) = tokio::sync::mpsc::channel(DOWNLOAD_CHANNEL_CAPACITY);
        let dest_for_task = dest.clone();
        let extractor = tokio::task::spawn_blocking(move || {
            extract_single_file(ChannelReader::new(rx), "dump.rdb.gz", &dest_for_task)
        });
        // Odd-sized chunks, so tar headers straddle chunk boundaries.
        for chunk in archive.chunks(1_337) {
            if tx.send(Ok(Bytes::copy_from_slice(chunk))).await.is_err() {
                break;
            }
        }
        drop(tx);

        let extracted = extractor.await.expect("join");
        assert_eq!(extracted.result, Ok(payload.len() as u64));
        assert_eq!(std::fs::read(&dest).expect("dest"), payload);
    }

    #[tokio::test]
    async fn channel_reader_surfaces_a_download_error() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let archive = tar_with(&[("dump.rdb.gz", vec![1_u8; 8192].as_slice())]);

        let (tx, rx) = tokio::sync::mpsc::channel(DOWNLOAD_CHANNEL_CAPACITY);
        let dest_for_task = dest.clone();
        let extractor = tokio::task::spawn_blocking(move || {
            extract_single_file(ChannelReader::new(rx), "dump.rdb.gz", &dest_for_task)
        });
        tx.send(Ok(Bytes::copy_from_slice(&archive[..1024])))
            .await
            .expect("first chunk");
        tx.send(Err(std::io::Error::other("connection reset by daemon")))
            .await
            .expect("error chunk");
        drop(tx);

        let extracted = extractor.await.expect("join");
        let reason = extracted.result.expect_err("download broke");
        assert!(reason.contains("connection reset by daemon"), "{reason}");
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
