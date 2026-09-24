// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-stream write-ahead log for unsealed head lines (ADR-046 §1, §8a.2).
//!
//! The writer keeps up to [`crate::chunk::MAX_FLUSH_AGE_SECS`] of lines per
//! container in memory before sealing a chunk. The WAL makes that window
//! crash-safe: every line is appended (and periodically fsynced) to
//! `<TEMPS_DATA_DIR>/logs/wal/<container>.wal` as it arrives, and on restart
//! [`WalDir::recover`] replays surviving files back into [`LogLine`]s so they
//! can be sealed into chunks before anything is lost. Once a chunk is sealed
//! *and its manifest row is committed* (idempotent commit, §8a.2), the
//! sealed generation is removed. New appends use a separate active file,
//! so recovery never merges a committed chunk with its uncommitted tail.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tracing::{info, warn};

use crate::error::LogAggregatorError;
use crate::types::LogLine;

const RECORD_HEADER_LEN: usize = 8; // u32 len + u32 crc32
pub(crate) const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

/// On-disk size at which the writer seals a stream no matter what its
/// in-memory head accounting says. The head cap counts message bytes plus a
/// fixed per-line overhead, while a WAL record is the whole line as JSON
/// (labels, container id, escaped message) — routinely 3x larger, and
/// unbounded in ratio for short or escape-heavy lines. Bounding generations
/// in the WAL's own unit is what lets [`WHOLE_GENERATION_LIMIT`] promise that
/// every generation this writer produces is replayable. Matches the largest
/// head buffer the settings API accepts, so it never seals earlier than the
/// head cap on ordinary lines.
pub const MAX_GENERATION_BYTES: u64 = 256 * 1024 * 1024;

/// Largest immutable generation recovery replays in one piece (it must, so
/// the replayed chunk has the original's bytes and storage key). The writer
/// seals once a generation reaches [`MAX_GENERATION_BYTES`], so the append
/// that crossed it overshoots by at most one record.
const WHOLE_GENERATION_LIMIT: u64 =
    MAX_GENERATION_BYTES + (RECORD_HEADER_LEN + MAX_RECORD_BYTES) as u64;

/// Subdirectory of the WAL root for generations recovery could not replay in
/// full. Nothing in it is ever replayed automatically, which is what lets
/// collection and purge resume while it is non-empty: a purged line can only
/// come back if an operator moves a file back into the WAL root.
pub const DEFERRED_DIR: &str = "deferred";

/// Sidecar next to each deferred generation explaining why it is there.
const DEFERRED_REASON_EXTENSION: &str = "reason";

/// A container's lines recovered from its WAL file after a restart.
#[derive(Debug, Clone)]
pub struct RecoveredStream {
    pub container_id: String,
    pub lines: Vec<LogLine>,
    pub generation: Option<WalGeneration>,
}

/// What happens to a generation's file once the chunk sealed from it commits.
#[derive(Debug, Clone)]
enum AfterCommit {
    /// Everything in it is committed.
    Remove,
    /// More batches from the same file follow; the last one decides.
    Retain,
    /// Its replayable prefix is committed but the rest is unreadable: move it
    /// to [`DEFERRED_DIR`] with this explanation.
    Defer(String),
}

/// Immutable WAL input for exactly one seal. The bloom choice is persisted
/// in its filename so replay produces the same encoded bytes and storage key.
#[derive(Debug, Clone)]
pub struct WalGeneration {
    path: PathBuf,
    pub with_bloom: bool,
    after_commit: AfterCommit,
    recovery_identity: Option<String>,
}

impl WalGeneration {
    pub async fn remove(&self) -> Result<(), LogAggregatorError> {
        match &self.after_commit {
            AfterCommit::Retain => Ok(()),
            AfterCommit::Defer(reason) => defer_file(&self.path, reason).await,
            AfterCommit::Remove => match fs::remove_file(&self.path).await {
                Ok(()) => sync_parent(&self.path).await,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            },
        }
    }

    pub fn recovery_identity(&self) -> Option<&str> {
        self.recovery_identity.as_deref()
    }
}

impl WalRecovery {
    fn new(root: PathBuf, paths: Vec<(PathBuf, bool, bool)>, batch_bytes: usize) -> Self {
        Self {
            root,
            paths: paths.into_iter().rev().collect(),
            batch_bytes: batch_bytes.max(1),
            whole_generation_limit: WHOLE_GENERATION_LIMIT,
            current: None,
        }
    }

    /// Tests shrink the limit instead of writing a quarter-gigabyte file.
    #[cfg(test)]
    fn with_whole_generation_limit(mut self, limit: u64) -> Self {
        self.whole_generation_limit = limit;
        self
    }

    /// Next batch to seal, or `None` once every file is replayed or deferred.
    ///
    /// A file that cannot be replayed in full never stops the others: its
    /// replayable prefix is returned for commit and the file itself moves to
    /// [`DEFERRED_DIR`], so one damaged generation cannot pause collection
    /// for every container on the host.
    pub async fn next(&mut self) -> Result<Option<RecoveredStream>, LogAggregatorError> {
        loop {
            if self.current.is_none() {
                let Some((path, with_bloom, batchable)) = self.paths.pop() else {
                    return Ok(None);
                };
                let file_len = fs::metadata(&path).await?.len();
                if !batchable && file_len > self.whole_generation_limit {
                    let reason = format!(
                        "sealed generation is {file_len} bytes, larger than the {} bytes recovery \
                         replays in one piece, so nothing was replayed from it. {}",
                        self.whole_generation_limit,
                        retry_instructions(&self.root)
                    );
                    defer_file(&path, &reason).await?;
                    continue;
                }
                self.current = Some(RecoveryFile {
                    reader: BufReader::new(File::open(&path).await?),
                    generation: WalGeneration {
                        path,
                        with_bloom,
                        after_commit: AfterCommit::Retain,
                        recovery_identity: None,
                    },
                    offset: 0,
                    pending_header: None,
                    batchable,
                    records: 0,
                    unreadable: None,
                });
            }

            let current = self
                .current
                .as_mut()
                .ok_or_else(|| LogAggregatorError::Validation {
                    message: "WAL recovery cursor lost its current file".to_string(),
                })?;
            let batch = match read_batch(current, self.batch_bytes).await {
                Ok(batch) => batch,
                Err(error @ LogAggregatorError::WalRecoveryReadFailed { .. }) => {
                    // The file cannot be read at all past this point. Batches
                    // already committed from it replay idempotently if it is
                    // ever moved back, so deferring the whole file loses
                    // nothing and unblocks every other stream.
                    let path = current.generation.path.clone();
                    self.current = None;
                    let reason = format!("{error}. {}", retry_instructions(&self.root));
                    defer_file(&path, &reason).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };

            if batch.lines.is_empty() {
                let mut generation = current.generation.clone();
                generation.after_commit = match &current.unreadable {
                    None => AfterCommit::Remove,
                    Some(problem) => AfterCommit::Defer(format!(
                        "{problem}; no records were replayed from it. {}",
                        retry_instructions(&self.root)
                    )),
                };
                self.current = None;
                generation.remove().await?;
                continue;
            }
            let container_id = batch.lines[0].container_id.clone();
            let mut generation = current.generation.clone();
            generation.after_commit = match (batch.reached_end, &current.unreadable) {
                (false, _) => AfterCommit::Retain,
                (true, None) => AfterCommit::Remove,
                (true, Some(problem)) => AfterCommit::Defer(format!(
                    "{problem}; the {} records before it were replayed. {}",
                    current.records,
                    retry_instructions(&self.root)
                )),
            };
            if current.batchable {
                let start = batch.start.ok_or_else(|| LogAggregatorError::Validation {
                    message: format!(
                        "WAL '{}' recovered a non-empty batch without a byte offset",
                        current.generation.path.display()
                    ),
                })?;
                generation.recovery_identity = Some(format!(
                    "{}:{start}:{}",
                    current
                        .generation
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("recovery-wal"),
                    current.offset
                ));
            }
            if batch.reached_end {
                self.current = None;
            }
            return Ok(Some(RecoveredStream {
                container_id,
                lines: batch.lines,
                generation: Some(generation),
            }));
        }
    }
}

/// One bounded slice of a recovering file.
struct Batch {
    lines: Vec<LogLine>,
    /// Byte offset of the batch's first record.
    start: Option<u64>,
    /// No more records follow in this file (clean end, torn tail, or
    /// [`RecoveryFile::unreadable`]).
    reached_end: bool,
}

async fn read_batch(
    current: &mut RecoveryFile,
    batch_bytes: usize,
) -> Result<Batch, LogAggregatorError> {
    let mut lines = Vec::new();
    let mut payload_bytes = 0usize;
    let mut start = None;
    let reached_end = loop {
        let (len, crc, record_offset) = match current.pending_header.take() {
            Some(header) => header,
            None => match read_record_header(current).await? {
                Some(header) => header,
                None => break true,
            },
        };
        if len > MAX_RECORD_BYTES {
            current.unreadable = Some(format!(
                "record at byte {record_offset} claims {len} bytes, more than the \
                 {MAX_RECORD_BYTES}-byte maximum the writer ever appends"
            ));
            break true;
        }
        if current.batchable && !lines.is_empty() && payload_bytes.saturating_add(len) > batch_bytes
        {
            current.pending_header = Some((len, crc, record_offset));
            break false;
        }
        start.get_or_insert(record_offset);
        let mut payload = vec![0; len];
        if let Err(error) = current.reader.read_exact(&mut payload).await {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                log_torn_tail(current, record_offset, "payload");
                break true;
            }
            return Err(wal_read_error(current, record_offset, error));
        }
        current.offset += len as u64;
        if crc32fast::hash(&payload) != crc {
            current.unreadable = Some(format!("record at byte {record_offset} fails its checksum"));
            break true;
        }
        match serde_json::from_slice::<LogLine>(&payload) {
            Ok(line) => lines.push(line),
            Err(error) => {
                current.unreadable = Some(format!(
                    "record at byte {record_offset} is not a log line ({error})"
                ));
                break true;
            }
        }
        current.records += 1;
        payload_bytes = payload_bytes.saturating_add(len);
    };
    if let Some(problem) = &current.unreadable {
        warn!(
            path = %current.generation.path.display(),
            problem = %problem,
            replayed_records = current.records,
            "wal: stopping recovery for this file; it will be deferred once its readable prefix commits"
        );
    }
    Ok(Batch {
        lines,
        start,
        reached_end,
    })
}

/// A record cut short by end-of-file is the write a crash interrupted. The
/// bytes that would complete it do not exist anywhere, so keeping the file
/// would preserve nothing: the readable prefix replays and the file is
/// removed like a clean one.
fn log_torn_tail(current: &RecoveryFile, record_offset: u64, part: &str) {
    info!(
        path = %current.generation.path.display(),
        offset = record_offset,
        replayed_records = current.records,
        "wal: discarding the torn final record {part} left by an interrupted write"
    );
}

async fn read_record_header(
    current: &mut RecoveryFile,
) -> Result<Option<(usize, u32, u64)>, LogAggregatorError> {
    let record_offset = current.offset;
    let mut header = [0u8; RECORD_HEADER_LEN];
    let read = current
        .reader
        .read(&mut header[..1])
        .await
        .map_err(|error| wal_read_error(current, record_offset, error))?;
    if read == 0 {
        return Ok(None);
    }
    if let Err(error) = current.reader.read_exact(&mut header[1..]).await {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            log_torn_tail(current, record_offset, "header");
            return Ok(None);
        }
        return Err(wal_read_error(current, record_offset, error));
    }
    current.offset += RECORD_HEADER_LEN as u64;
    let len = u32::from_le_bytes(header[0..4].try_into().unwrap_or_default()) as usize;
    let crc = u32::from_le_bytes(header[4..8].try_into().unwrap_or_default());
    Ok(Some((len, crc, record_offset)))
}

fn wal_read_error(
    current: &RecoveryFile,
    offset: u64,
    error: std::io::Error,
) -> LogAggregatorError {
    LogAggregatorError::WalRecoveryReadFailed {
        path: current.generation.path.display().to_string(),
        offset,
        reason: error.to_string(),
    }
}

fn retry_instructions(root: &Path) -> String {
    format!(
        "Log collection continued without it and it is never replayed automatically; to \
         retry, move it back into {} and restart temps (lines purged since would come back)",
        root.display()
    )
}

/// Move `path` into [`DEFERRED_DIR`] beside a note saying why. The note is
/// written first, so a crash in between leaves an unexplained note rather
/// than an unexplained file.
async fn defer_file(path: &Path, reason: &str) -> Result<(), LogAggregatorError> {
    let (Some(root), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return Err(LogAggregatorError::Validation {
            message: format!(
                "WAL path '{}' has no parent directory or file name",
                path.display()
            ),
        });
    };
    let dir = root.join(DEFERRED_DIR);
    fs::create_dir_all(&dir).await?;
    let target = dir.join(name);
    fs::write(
        dir.join(format!("{name}.{DEFERRED_REASON_EXTENSION}")),
        reason,
    )
    .await?;
    fs::rename(path, &target).await?;
    sync_parent(&target).await?;
    sync_parent(path).await?;
    warn!(
        from = %path.display(),
        to = %target.display(),
        reason = %reason,
        "wal: deferred a generation recovery could not replay in full"
    );
    Ok(())
}

/// Longest reason note returned; ours are a few hundred bytes.
const MAX_REASON_BYTES: u64 = 4 * 1024;

/// Read a deferral note if it is a regular file, never through a symlink and
/// never more than [`MAX_REASON_BYTES`]. Anything else is ignored with a
/// warning rather than failing the listing: the note only explains, and the
/// generation it belongs to must still be reported.
async fn read_reason_note(
    path: &Path,
) -> Result<Option<(String, Option<std::time::SystemTime>)>, LogAggregatorError> {
    let linked = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !linked.file_type().is_file() {
        warn!(path = %path.display(), "deferred WAL reason note is not a regular file; ignoring it");
        return Ok(None);
    }
    let file = File::open(path).await?;
    // Close the window between the check and the open: the opened file must
    // be the very inode that was checked, not something swapped in since.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata().await?;
        if (opened.dev(), opened.ino()) != (linked.dev(), linked.ino()) {
            warn!(path = %path.display(), "deferred WAL reason note changed while being read; ignoring it");
            return Ok(None);
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_REASON_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    let truncated = bytes.len() as u64 > MAX_REASON_BYTES;
    bytes.truncate(MAX_REASON_BYTES as usize);
    let mut reason = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        reason.push('…');
    }
    Ok(Some((reason, linked.modified().ok())))
}

/// A generation parked in [`DEFERRED_DIR`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredGeneration {
    pub path: PathBuf,
    pub bytes: u64,
    pub deferred_at: Option<DateTime<Utc>>,
    /// `None` when the note is missing (e.g. an operator copied the file in).
    pub reason: Option<String>,
}

/// Bounded, file-at-a-time WAL recovery cursor.
pub struct WalRecovery {
    root: PathBuf,
    paths: Vec<(PathBuf, bool, bool)>,
    batch_bytes: usize,
    whole_generation_limit: u64,
    current: Option<RecoveryFile>,
}

struct RecoveryFile {
    reader: BufReader<File>,
    generation: WalGeneration,
    offset: u64,
    pending_header: Option<(usize, u32, u64)>,
    batchable: bool,
    /// Records read so far, across batches.
    records: u64,
    /// Why reading stopped before the end of the file, when it did.
    unreadable: Option<String>,
}

async fn sync_parent(path: &Path) -> Result<(), LogAggregatorError> {
    if let Some(parent) = path.parent() {
        File::open(parent).await?.sync_all().await?;
    }
    Ok(())
}

#[cfg(test)]
fn parse_generation(path: PathBuf) -> Result<WalGeneration, LogAggregatorError> {
    let with_bloom = match path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit('.').next())
    {
        Some("b") => true,
        Some("n") => false,
        _ => {
            return Err(LogAggregatorError::Validation {
                message: format!(
                    "invalid sealed WAL generation filename '{}'",
                    path.display()
                ),
            });
        }
    };
    Ok(WalGeneration {
        path,
        with_bloom,
        after_commit: AfterCommit::Remove,
        recovery_identity: None,
    })
}

/// Directory of per-stream WAL files.
pub struct WalDir {
    root: PathBuf,
}

impl WalDir {
    /// Background recovery must not enable purge while a deferred or damaged
    /// generation could replay previously deleted records on a later restart.
    pub(crate) async fn ensure_recovery_complete(&self) -> Result<(), LogAggregatorError> {
        let mut entries = fs::read_dir(&self.root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("sealed-wal" | "recovery-wal")
            ) {
                return Err(LogAggregatorError::WalRecoveryIncomplete {
                    path: path.display().to_string(),
                });
            }
        }
        Ok(())
    }

    /// Open (creating if necessary) the WAL root directory.
    pub async fn open(root: PathBuf) -> Result<Self, LogAggregatorError> {
        fs::create_dir_all(&root).await?;
        Ok(WalDir { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Generations recovery moved to [`DEFERRED_DIR`], oldest deferral first.
    /// Read-only: listing never moves, deletes, or replays anything.
    pub async fn deferred(&self) -> Result<Vec<DeferredGeneration>, LogAggregatorError> {
        let dir = self.root.join(DEFERRED_DIR);
        // Listed over an authenticated API, so nothing here may be steered
        // elsewhere by a planted symlink: not the directory, not a note.
        match fs::symlink_metadata(&dir).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                warn!(path = %dir.display(), "deferred WAL path is not a real directory; not listing it");
                return Ok(Vec::new());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
        let mut entries = fs::read_dir(&dir).await?;
        let mut deferred = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let metadata = entry.metadata().await?;
            if !metadata.is_file()
                || path.extension().and_then(|extension| extension.to_str())
                    == Some(DEFERRED_REASON_EXTENSION)
            {
                continue;
            }
            let note = dir.join(format!(
                "{}.{DEFERRED_REASON_EXTENSION}",
                entry.file_name().to_string_lossy()
            ));
            // The note is written at deferral time; the generation keeps the
            // mtime of its last append through the rename.
            let (reason, deferred_at) = match read_reason_note(&note).await? {
                Some((reason, written_at)) => (Some(reason), written_at),
                None => (None, metadata.modified().ok()),
            };
            deferred.push(DeferredGeneration {
                path,
                bytes: metadata.len(),
                deferred_at: deferred_at.map(DateTime::<Utc>::from),
                reason,
            });
        }
        deferred.sort_by(|a, b| a.deferred_at.cmp(&b.deferred_at).then(a.path.cmp(&b.path)));
        Ok(deferred)
    }

    /// Open (creating if necessary) the append-only WAL file for
    /// `container_id`.
    pub async fn stream(&self, container_id: &str) -> Result<StreamWal, LogAggregatorError> {
        let path = self.root.join(wal_file_name(container_id));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        let bytes_written = file.metadata().await?.len();
        Ok(StreamWal {
            path,
            writer: Some(BufWriter::new(file)),
            frozen: None,
            bytes_written,
        })
    }

    /// Freeze active tails and return a cursor that decodes at most
    /// `batch_bytes` of record payload at a time. A single record larger than
    /// the limit is returned alone. Source generations are retained until
    /// their final batch commits, making partial recovery retries idempotent.
    pub async fn recover_batched(
        &self,
        batch_bytes: usize,
        with_bloom: bool,
    ) -> Result<WalRecovery, LogAggregatorError> {
        let mut paths = Vec::new();
        let mut active_paths = Vec::new();
        let mut entries = match fs::read_dir(&self.root).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(WalRecovery::new(self.root.clone(), paths, batch_bytes))
            }
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("wal") => {
                    if !entry.file_type().await?.is_file() {
                        return Err(LogAggregatorError::Validation {
                            message: format!(
                                "WAL path '{}' is not a regular file; refusing to block startup while reading it",
                                path.display()
                            ),
                        });
                    }
                    if entry.metadata().await?.len() > 0 {
                        active_paths.push(path);
                    }
                }
                Some("sealed-wal") | Some("recovery-wal") => {
                    if !entry.file_type().await?.is_file() {
                        return Err(LogAggregatorError::Validation {
                            message: format!(
                                "WAL generation '{}' is not a regular file",
                                path.display()
                            ),
                        });
                    }
                    let batchable = path.extension().and_then(|extension| extension.to_str())
                        == Some("recovery-wal");
                    let generation_with_bloom = match path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.rsplit('.').next())
                    {
                        Some("b") => true,
                        Some("n") => false,
                        _ => {
                            return Err(LogAggregatorError::Validation {
                                message: format!(
                                    "invalid sealed WAL generation filename '{}'",
                                    path.display()
                                ),
                            })
                        }
                    };
                    paths.push((path, generation_with_bloom, batchable));
                }
                _ => continue,
            }
        }

        // Existing immutable generations must replay before their active tail.
        paths.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        active_paths.sort();
        if !active_paths.is_empty() {
            warn!(
                active_wal_count = active_paths.len(),
                "wal: migrating active WAL files to bounded recovery; files created before immutable generations were introduced replay with at-least-once semantics"
            );
        }
        for active_path in active_paths {
            let suffix = if with_bloom { "b" } else { "n" };
            let generation_path = active_path.with_extension(format!(
                "{}.{}.recovery-wal",
                uuid::Uuid::new_v4(),
                suffix
            ));
            fs::rename(&active_path, &generation_path).await?;
            sync_parent(&active_path).await?;
            paths.push((generation_path, with_bloom, true));
        }
        Ok(WalRecovery::new(self.root.clone(), paths, batch_bytes))
    }

    #[cfg(test)]
    pub async fn recover(&self) -> Result<Vec<RecoveredStream>, LogAggregatorError> {
        let mut out = Vec::new();
        let mut entries = fs::read_dir(&self.root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let generation = match path.extension().and_then(|extension| extension.to_str()) {
                Some("wal") => None,
                Some("sealed-wal") => Some(parse_generation(path.clone())?),
                _ => continue,
            };
            let lines = read_records(&path).await?;
            if let Some(first) = lines.first() {
                out.push(RecoveredStream {
                    container_id: first.container_id.clone(),
                    lines,
                    generation,
                });
            }
        }
        out.sort_by_key(|stream| stream.generation.is_none());
        Ok(out)
    }

    /// Delete the active WAL file for `container_id`, if any. Frozen
    /// generations are removed individually only after successful commits.
    pub async fn remove(&self, container_id: &str) -> Result<(), LogAggregatorError> {
        let path = self.root.join(wal_file_name(container_id));
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// An open, append-only WAL file for one container's stream.
pub struct StreamWal {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    frozen: Option<WalGeneration>,
    bytes_written: u64,
}

impl StreamWal {
    fn writer(&mut self) -> Result<&mut BufWriter<File>, LogAggregatorError> {
        self.writer.as_mut().ok_or_else(|| LogAggregatorError::Validation {
            message: format!("WAL '{}' is frozen after an incomplete rotation; retry sealing before appending", self.path.display()),
        })
    }

    /// Freeze exactly the current head. On a reopen/fsync error the pending
    /// generation stays attached and appends fail closed; retrying rotation
    /// resumes from that boundary rather than renaming or extending it again.
    async fn freeze(&mut self, with_bloom: bool) -> Result<(), LogAggregatorError> {
        if self.frozen.is_none() {
            self.sync().await?;
            let suffix = if with_bloom { "b" } else { "n" };
            let path =
                self.path
                    .with_extension(format!("{}.{}.sealed-wal", uuid::Uuid::new_v4(), suffix));
            fs::rename(&self.path, &path).await?;
            self.writer = None;
            self.frozen = Some(WalGeneration {
                path,
                with_bloom,
                after_commit: AfterCommit::Remove,
                recovery_identity: None,
            });
        }
        Ok(())
    }

    pub async fn rotate(&mut self, with_bloom: bool) -> Result<WalGeneration, LogAggregatorError> {
        self.freeze(with_bloom).await?;
        sync_parent(&self.path).await?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        // Persist the fresh active directory entry before ingest can append.
        sync_parent(&self.path).await?;
        self.writer = Some(BufWriter::new(file));
        self.bytes_written = 0;
        self.frozen
            .take()
            .ok_or_else(|| LogAggregatorError::Validation {
                message: format!(
                    "WAL '{}' lost its frozen generation during rotation",
                    self.path.display()
                ),
            })
    }

    /// Append one record. Buffered — no fsync; call [`Self::sync`]
    /// periodically (the writer does this on a 1 s tick) for durability.
    pub async fn append(&mut self, line: &LogLine) -> Result<(), LogAggregatorError> {
        let payload = serde_json::to_vec(line)?;
        if payload.len() > MAX_RECORD_BYTES {
            return Err(LogAggregatorError::Validation {
                message: format!(
                    "WAL record for container '{}' is {} bytes; maximum is {MAX_RECORD_BYTES}",
                    line.container_id,
                    payload.len()
                ),
            });
        }
        let crc = crc32fast::hash(&payload);
        let len = u32::try_from(payload.len()).map_err(|_| LogAggregatorError::Validation {
            message: format!(
                "WAL record for container '{}' is too large to encode",
                line.container_id
            ),
        })?;

        let mut header = [0u8; RECORD_HEADER_LEN];
        header[0..4].copy_from_slice(&len.to_le_bytes());
        header[4..8].copy_from_slice(&crc.to_le_bytes());

        self.writer()?.write_all(&header).await?;
        self.writer()?.write_all(&payload).await?;
        self.bytes_written += (RECORD_HEADER_LEN + payload.len()) as u64;
        Ok(())
    }

    /// Flush the buffered writer and fsync the underlying file.
    pub async fn sync(&mut self) -> Result<(), LogAggregatorError> {
        self.writer()?.flush().await?;
        self.writer()?.get_ref().sync_data().await?;
        Ok(())
    }

    /// Truncate the WAL back to empty. Callers must only do this after the
    /// chunk sealed from this stream's buffered lines has both been written
    /// to object storage *and* had its manifest row committed — otherwise a
    /// crash between those two steps loses the lines this WAL was protecting.
    pub async fn truncate(&mut self) -> Result<(), LogAggregatorError> {
        self.writer()?.flush().await?;
        self.writer()?.get_ref().set_len(0).await?;
        self.writer()?.get_ref().sync_data().await?;
        // BufWriter has no seek-to-start primitive of its own beyond the
        // inner file; since the file is append-mode, writes always land at
        // the (now zero) end of file, so no explicit seek is needed.
        self.bytes_written = 0;
        Ok(())
    }

    /// Bytes appended since the WAL was opened or last truncated (including
    /// record headers).
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }
}

/// Sanitise a container id into a safe file name component: keep
/// `[A-Za-z0-9_-]`, replace everything else with `_`.
fn sanitise(container_id: &str) -> String {
    container_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn wal_file_name(container_id: &str) -> String {
    format!("{}.wal", sanitise(container_id))
}

/// Read every valid `[len][crc32][payload]` record from `path` in order,
/// stopping (and warning) at the first truncated or corrupt record.
#[cfg(test)]
async fn read_records(path: &Path) -> Result<Vec<LogLine>, LogAggregatorError> {
    let mut file = File::open(path).await?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await?;

    let mut lines = Vec::new();
    let mut offset = 0usize;

    while offset < buf.len() {
        if offset + RECORD_HEADER_LEN > buf.len() {
            warn!(
                path = %path.display(),
                "wal: truncated record header at offset {offset}, stopping recovery for this file"
            );
            break;
        }
        let len =
            u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap_or_default()) as usize;
        let crc = u32::from_le_bytes(buf[offset + 4..offset + 8].try_into().unwrap_or_default());
        let payload_start = offset + RECORD_HEADER_LEN;
        let payload_end = payload_start + len;
        if payload_end > buf.len() {
            warn!(
                path = %path.display(),
                "wal: truncated record payload at offset {offset} (need {len} bytes), stopping recovery for this file"
            );
            break;
        }
        let payload = &buf[payload_start..payload_end];
        let actual_crc = crc32fast::hash(payload);
        if actual_crc != crc {
            warn!(
                path = %path.display(),
                "wal: crc mismatch at offset {offset}, stopping recovery for this file"
            );
            break;
        }
        match serde_json::from_slice::<LogLine>(payload) {
            Ok(line) => lines.push(line),
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "wal: failed to deserialize record at offset {offset}, stopping recovery for this file"
                );
                break;
            }
        }
        offset = payload_end;
    }

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LogLevel, LogStream};
    use chrono::Utc;

    fn sample_line(container_id: &str, msg: &str) -> LogLine {
        LogLine {
            ts: Utc::now(),
            stream: LogStream::Stdout,
            level: LogLevel::Info,
            msg: msg.to_string(),
            fields: None,
            container_id: container_id.to_string(),
            service: "web".to_string(),
            env: "1".to_string(),
            project_id: 42,
            external_service_id: None,
            deploy_id: None,
            node_id: None,
            node_name: None,
        }
    }

    #[tokio::test]
    async fn rotated_generation_and_active_tail_recover_independently() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = WalDir::open(dir.path().to_owned()).await.unwrap();
        let mut stream = wal_dir.stream("rotated").await.unwrap();
        stream.append(&sample_line("rotated", "A")).await.unwrap();
        let generation = stream.rotate(false).await.unwrap();
        stream.append(&sample_line("rotated", "B")).await.unwrap();
        stream.sync().await.unwrap();
        let recovered = wal_dir.recover().await.unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].lines[0].msg, "A");
        assert!(!recovered[0].generation.as_ref().unwrap().with_bloom);
        assert_eq!(recovered[1].lines[0].msg, "B");
        assert!(recovered[1].generation.is_none());
        generation.remove().await.unwrap();
        generation.remove().await.unwrap();
        let recovered = wal_dir.recover().await.unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].lines[0].msg, "B");
    }

    #[tokio::test]
    async fn recovery_after_freeze_before_active_reopen_preserves_generation() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = WalDir::open(dir.path().to_owned()).await.unwrap();
        let mut stream = wal_dir.stream("freeze-crash").await.unwrap();
        stream
            .append(&sample_line("freeze-crash", "A"))
            .await
            .unwrap();
        stream.freeze(true).await.unwrap();
        assert!(!stream.path.exists());
        drop(stream);
        let recovered = wal_dir.recover().await.unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].lines[0].msg, "A");
        assert!(recovered[0].generation.as_ref().unwrap().with_bloom);
    }

    #[tokio::test]
    async fn failed_reopen_rejects_appends_and_retry_keeps_original_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let wal_dir = WalDir::open(dir.path().to_owned()).await.unwrap();
        let mut stream = wal_dir.stream("reopen-failure").await.unwrap();
        stream
            .append(&sample_line("reopen-failure", "A"))
            .await
            .unwrap();
        stream.freeze(false).await.unwrap();
        let frozen_path = stream.frozen.as_ref().unwrap().path.clone();
        fs::create_dir(&stream.path).await.unwrap();
        assert!(stream.rotate(true).await.is_err());
        assert!(stream
            .append(&sample_line("reopen-failure", "must not append"))
            .await
            .is_err());
        assert_eq!(read_records(&frozen_path).await.unwrap().len(), 1);
        fs::remove_dir(&stream.path).await.unwrap();
        let generation = stream.rotate(true).await.unwrap();
        assert_eq!(generation.path, frozen_path);
        assert!(
            !generation.with_bloom,
            "retry preserves the original generation's encoding mode"
        );
        stream
            .append(&sample_line("reopen-failure", "B"))
            .await
            .unwrap();
        stream.sync().await.unwrap();
        let recovered = wal_dir.recover().await.unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].lines[0].msg, "A");
        assert_eq!(recovered[1].lines[0].msg, "B");
    }

    #[tokio::test]
    async fn failed_rename_leaves_active_wal_writable_and_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("wal");
        let moved = dir.path().join("moved");
        let wal_dir = WalDir::open(root.clone()).await.unwrap();
        let mut stream = wal_dir.stream("rename-failure").await.unwrap();
        stream
            .append(&sample_line("rename-failure", "A"))
            .await
            .unwrap();
        fs::rename(&root, &moved).await.unwrap();
        fs::write(&root, b"not a directory").await.unwrap();
        assert!(stream.rotate(true).await.is_err());
        stream
            .append(&sample_line("rename-failure", "B"))
            .await
            .unwrap();
        stream.sync().await.unwrap();
        fs::remove_file(&root).await.unwrap();
        fs::rename(moved, root).await.unwrap();
        let recovered = wal_dir.recover().await.unwrap();
        assert_eq!(recovered[0].lines.len(), 2);
    }

    #[tokio::test]
    async fn append_sync_recover_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");

        let mut stream = wal_dir.stream("container-a").await.expect("stream");
        stream
            .append(&sample_line("container-a", "line 1"))
            .await
            .expect("append");
        stream
            .append(&sample_line("container-a", "line 2"))
            .await
            .expect("append");
        stream.sync().await.expect("sync");

        let recovered = wal_dir.recover().await.expect("recover");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].container_id, "container-a");
        assert_eq!(recovered[0].lines.len(), 2);
        assert_eq!(recovered[0].lines[0].msg, "line 1");
        assert_eq!(recovered[0].lines[1].msg, "line 2");
    }

    #[tokio::test]
    async fn truncated_last_record_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");

        let mut stream = wal_dir.stream("container-b").await.expect("stream");
        stream
            .append(&sample_line("container-b", "good line"))
            .await
            .expect("append");
        stream.sync().await.expect("sync");

        // Manually append a truncated record: valid header claiming more
        // payload bytes than actually follow.
        let path = stream.path().to_path_buf();
        drop(stream);
        let mut f = OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open");
        let mut garbage = Vec::new();
        garbage.extend_from_slice(&1000u32.to_le_bytes());
        garbage.extend_from_slice(&0u32.to_le_bytes());
        garbage.extend_from_slice(b"short");
        f.write_all(&garbage).await.expect("write garbage");
        f.flush().await.expect("flush");

        let recovered = wal_dir.recover().await.expect("recover");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].lines.len(), 1);
        assert_eq!(recovered[0].lines[0].msg, "good line");
    }

    #[tokio::test]
    async fn corrupt_crc_record_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");

        let mut stream = wal_dir.stream("container-c").await.expect("stream");
        stream
            .append(&sample_line("container-c", "first"))
            .await
            .expect("append");
        stream.sync().await.expect("sync");

        let path = stream.path().to_path_buf();
        drop(stream);

        // Append a record with a bad crc for a valid-length payload.
        let payload = serde_json::to_vec(&sample_line("container-c", "second")).expect("json");
        let mut f = OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open");
        let mut record = Vec::new();
        record.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        record.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // wrong crc
        record.extend_from_slice(&payload);
        f.write_all(&record).await.expect("write");
        f.flush().await.expect("flush");

        let recovered = wal_dir.recover().await.expect("recover");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].lines.len(), 1);
        assert_eq!(recovered[0].lines[0].msg, "first");
    }

    #[tokio::test]
    async fn truncate_resets_stream() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");

        let mut stream = wal_dir.stream("container-d").await.expect("stream");
        stream
            .append(&sample_line("container-d", "line 1"))
            .await
            .expect("append");
        stream.sync().await.expect("sync");
        assert!(stream.bytes_written() > 0);

        stream.truncate().await.expect("truncate");
        assert_eq!(stream.bytes_written(), 0);

        stream
            .append(&sample_line("container-d", "line 2"))
            .await
            .expect("append");
        stream.sync().await.expect("sync");

        let recovered = wal_dir.recover().await.expect("recover");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].lines.len(), 1);
        assert_eq!(recovered[0].lines[0].msg, "line 2");
    }

    #[tokio::test]
    async fn recover_ignores_empty_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");

        // Creating a stream and never appending leaves an empty .wal file.
        let _stream = wal_dir.stream("container-e").await.expect("stream");

        let recovered = wal_dir.recover().await.expect("recover");
        assert!(recovered.is_empty());
    }

    #[tokio::test]
    async fn multiple_streams_recover_independently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");

        let mut s1 = wal_dir.stream("container-f").await.expect("stream");
        s1.append(&sample_line("container-f", "f1"))
            .await
            .expect("append");
        s1.sync().await.expect("sync");

        let mut s2 = wal_dir.stream("container-g").await.expect("stream");
        s2.append(&sample_line("container-g", "g1"))
            .await
            .expect("append");
        s2.append(&sample_line("container-g", "g2"))
            .await
            .expect("append");
        s2.sync().await.expect("sync");

        let mut recovered = wal_dir.recover().await.expect("recover");
        recovered.sort_by(|a, b| a.container_id.cmp(&b.container_id));
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].container_id, "container-f");
        assert_eq!(recovered[0].lines.len(), 1);
        assert_eq!(recovered[1].container_id, "container-g");
        assert_eq!(recovered[1].lines.len(), 2);
    }

    #[tokio::test]
    async fn batched_recovery_bounds_a_single_large_wal_and_removes_only_at_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir.stream("large").await.expect("stream");
        for index in 0..40 {
            stream
                .append(&sample_line(
                    "large",
                    &format!("{index:04}-{}", "x".repeat(4096)),
                ))
                .await
                .expect("append");
        }
        stream.sync().await.expect("sync");
        drop(stream);

        let mut recovery = wal_dir
            .recover_batched(20 * 1024, true)
            .await
            .expect("start recovery");
        let first = recovery.next().await.expect("first batch").expect("batch");
        assert!(first.lines.len() < 40, "oversized WAL must be split");
        let generation_path = first
            .generation
            .as_ref()
            .expect("frozen generation")
            .path
            .clone();
        let mut recovered = first.lines.len();
        first
            .generation
            .expect("generation")
            .remove()
            .await
            .expect("non-final cleanup");
        assert!(generation_path.exists(), "non-final batch retains source");

        let mut batches = 1;
        while let Some(batch) = recovery.next().await.expect("next batch") {
            recovered += batch.lines.len();
            batches += 1;
            batch
                .generation
                .expect("generation")
                .remove()
                .await
                .expect("cleanup");
        }
        assert!(batches > 1);
        assert_eq!(recovered, 40);
        assert!(!generation_path.exists(), "final batch removes source");
    }

    #[tokio::test]
    async fn interrupted_batched_recovery_replays_from_source_without_loss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir.stream("restart").await.expect("stream");
        for index in 0..12 {
            stream
                .append(&sample_line(
                    "restart",
                    &format!("line-{index}-{}", "y".repeat(2048)),
                ))
                .await
                .expect("append");
        }
        stream.sync().await.expect("sync");
        drop(stream);

        let mut first_attempt = wal_dir
            .recover_batched(8 * 1024, false)
            .await
            .expect("first recovery");
        let committed_prefix = first_attempt
            .next()
            .await
            .expect("first batch")
            .expect("batch");
        committed_prefix
            .generation
            .expect("generation")
            .remove()
            .await
            .expect("non-final cleanup");
        drop(first_attempt); // simulated process failure after the first commit

        let mut retry = wal_dir
            .recover_batched(8 * 1024, false)
            .await
            .expect("retry recovery");
        let mut messages = Vec::new();
        while let Some(batch) = retry.next().await.expect("retry batch") {
            messages.extend(batch.lines.iter().map(|line| line.msg.clone()));
            batch
                .generation
                .expect("generation")
                .remove()
                .await
                .expect("cleanup");
        }
        assert_eq!(messages.len(), 12);
        assert_eq!(messages[0], format!("line-0-{}", "y".repeat(2048)));
        assert_eq!(messages[11], format!("line-11-{}", "y".repeat(2048)));
    }

    #[tokio::test]
    async fn immutable_generations_recover_before_batched_active_tails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir.stream("ordered").await.expect("stream");
        stream
            .append(&sample_line("ordered", "immutable"))
            .await
            .expect("append immutable");
        stream.rotate(false).await.expect("rotate");
        stream
            .append(&sample_line("ordered", "active tail"))
            .await
            .expect("append tail");
        stream.sync().await.expect("sync");
        drop(stream);

        let mut recovery = wal_dir.recover_batched(1024, true).await.expect("recovery");
        assert_eq!(
            recovery.next().await.unwrap().unwrap().lines[0].msg,
            "immutable"
        );
        assert_eq!(
            recovery.next().await.unwrap().unwrap().lines[0].msg,
            "active tail"
        );
    }

    #[tokio::test]
    async fn append_rejects_records_larger_than_the_recovery_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir.stream("oversized-record").await.expect("stream");
        let error = stream
            .append(&sample_line(
                "oversized-record",
                &"x".repeat(MAX_RECORD_BYTES),
            ))
            .await
            .expect_err("oversized record must be rejected");
        assert!(error.to_string().contains("oversized-record"));
        assert_eq!(stream.bytes_written(), 0);
    }

    /// Every name in `dir` with the given extension.
    async fn files_with_extension(dir: &Path, extension: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(mut entries) = fs::read_dir(dir).await else {
            return found;
        };
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry.path().extension().and_then(|value| value.to_str()) == Some(extension) {
                found.push(entry.path());
            }
        }
        found
    }

    /// Replay every batch, committing each (the writer's contract).
    async fn replay_all(recovery: &mut WalRecovery) -> Vec<String> {
        let mut messages = Vec::new();
        while let Some(batch) = recovery.next().await.expect("recovery batch") {
            messages.extend(batch.lines.iter().map(|line| line.msg.clone()));
            batch
                .generation
                .expect("generation")
                .remove()
                .await
                .expect("post-commit cleanup");
        }
        messages
    }

    /// Regression: a size-triggered seal of escape-heavy lines (ANSI colour
    /// codes, quotes, escaped newlines — a typical SQL query log) wrote a
    /// generation roughly three times the 8 MiB head cap, because the cap
    /// counts message bytes and the WAL stores whole lines as JSON. Recovery
    /// only replayed generations up to cap + 16 MiB, so every such generation
    /// was skipped, reported as needing repair, and kept collection paused.
    #[tokio::test]
    async fn generation_larger_than_head_cap_plus_one_record_replays_whole() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir.stream("query-log").await.expect("stream");
        let old_limit = (crate::chunk::DEFAULT_HEAD_MAX_BYTES + MAX_RECORD_BYTES) as u64;
        let line = sample_line("query-log", &"\x1b[2m\\n\"".repeat(1024));
        let mut appended = 0usize;
        while stream.bytes_written() <= old_limit {
            stream.append(&line).await.expect("append");
            appended += 1;
        }
        let generation_path = stream.rotate(true).await.expect("rotate").path.clone();
        drop(stream);

        let mut recovery = wal_dir
            .recover_batched(crate::chunk::DEFAULT_HEAD_MAX_BYTES, true)
            .await
            .expect("recovery cursor");
        let first = recovery.next().await.expect("recovery").expect("batch");
        assert_eq!(first.lines.len(), appended, "replayed whole, not split");
        first
            .generation
            .expect("generation")
            .remove()
            .await
            .expect("cleanup");
        assert!(recovery.next().await.expect("recovery end").is_none());
        assert!(!generation_path.exists());
        assert!(wal_dir.deferred().await.expect("deferred").is_empty());
        wal_dir
            .ensure_recovery_complete()
            .await
            .expect("nothing left behind");
    }

    #[tokio::test]
    async fn oversized_immutable_generation_is_deferred_without_blocking_others() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir
            .stream("oversized-generation")
            .await
            .expect("stream");
        for _ in 0..4 {
            stream
                .append(&sample_line("oversized-generation", &"x".repeat(1024)))
                .await
                .expect("append");
        }
        let generation = stream.rotate(true).await.expect("rotate");
        let file_name = generation.path.file_name().unwrap().to_owned();
        stream
            .append(&sample_line("oversized-generation", "small active tail"))
            .await
            .expect("append tail");
        stream.sync().await.expect("sync tail");
        drop(stream);

        let mut recovery = wal_dir
            .recover_batched(1024 * 1024, true)
            .await
            .expect("recovery cursor")
            .with_whole_generation_limit(1024);
        assert_eq!(replay_all(&mut recovery).await, vec!["small active tail"]);

        assert!(!generation.path.exists(), "moved out of the WAL root");
        wal_dir
            .ensure_recovery_complete()
            .await
            .expect("a deferred generation no longer blocks recovery");
        let deferred = wal_dir.deferred().await.expect("deferred");
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].path.file_name().unwrap(), file_name);
        let reason = deferred[0].reason.as_deref().expect("reason note");
        assert!(
            reason.contains("1024 bytes recovery replays in one piece"),
            "{reason}"
        );
        assert!(
            reason.contains(&dir.path().display().to_string()),
            "{reason}"
        );

        // Never replayed on a later start.
        let mut restart = wal_dir
            .recover_batched(1024 * 1024, true)
            .await
            .expect("restart");
        assert!(replay_all(&mut restart).await.is_empty());
    }

    #[tokio::test]
    async fn corrupt_record_defers_the_file_after_its_readable_prefix_commits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir.stream("corrupt").await.expect("stream");
        stream
            .append(&sample_line("corrupt", "first"))
            .await
            .expect("append");
        stream.sync().await.expect("sync");
        let path = stream.path().to_path_buf();
        drop(stream);
        let payload = serde_json::to_vec(&sample_line("corrupt", "second")).expect("json");
        let mut record = Vec::new();
        record.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        record.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        record.extend_from_slice(&payload);
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open");
        file.write_all(&record).await.expect("write");
        file.flush().await.expect("flush");

        let mut recovery = wal_dir
            .recover_batched(1024 * 1024, true)
            .await
            .expect("recovery");
        assert_eq!(replay_all(&mut recovery).await, vec!["first"]);

        assert!(files_with_extension(dir.path(), "recovery-wal")
            .await
            .is_empty());
        wal_dir
            .ensure_recovery_complete()
            .await
            .expect("recovery completes");
        let deferred = wal_dir.deferred().await.expect("deferred");
        assert_eq!(deferred.len(), 1);
        let reason = deferred[0].reason.as_deref().expect("reason note");
        assert!(reason.contains("fails its checksum"), "{reason}");
        assert!(
            reason.contains("the 1 records before it were replayed"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn torn_final_record_is_discarded_and_the_file_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir.stream("torn").await.expect("stream");
        stream
            .append(&sample_line("torn", "kept"))
            .await
            .expect("append");
        stream.sync().await.expect("sync");
        let path = stream.path().to_path_buf();
        drop(stream);
        // What a crash mid-append leaves: a header promising bytes that were
        // never written.
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open");
        file.write_all(&1000u32.to_le_bytes()).await.expect("write");
        file.write_all(&[0, 0, 0, 0, b's']).await.expect("write");
        file.flush().await.expect("flush");

        let mut recovery = wal_dir
            .recover_batched(1024 * 1024, true)
            .await
            .expect("recovery");
        assert_eq!(replay_all(&mut recovery).await, vec!["kept"]);
        assert!(files_with_extension(dir.path(), "recovery-wal")
            .await
            .is_empty());
        assert!(wal_dir.deferred().await.expect("deferred").is_empty());
        wal_dir
            .ensure_recovery_complete()
            .await
            .expect("recovery completes");
    }

    #[tokio::test]
    async fn deferred_generation_moved_back_replays_on_the_next_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir.stream("retry").await.expect("stream");
        stream
            .append(&sample_line("retry", &"x".repeat(2048)))
            .await
            .expect("append");
        stream.rotate(false).await.expect("rotate");
        drop(stream);
        let mut first = wal_dir
            .recover_batched(1024 * 1024, true)
            .await
            .expect("recovery")
            .with_whole_generation_limit(1024);
        assert!(replay_all(&mut first).await.is_empty());
        let deferred = wal_dir.deferred().await.expect("deferred");
        assert_eq!(deferred.len(), 1);

        // The operator's retry: move it back, restart with a build whose
        // limit covers it.
        let name = deferred[0].path.file_name().unwrap().to_owned();
        fs::rename(&deferred[0].path, dir.path().join(&name))
            .await
            .expect("move back");
        let mut second = wal_dir
            .recover_batched(1024 * 1024, true)
            .await
            .expect("recovery");
        assert_eq!(replay_all(&mut second).await, vec!["x".repeat(2048)]);
        assert!(wal_dir.deferred().await.expect("deferred").is_empty());
    }

    #[tokio::test]
    async fn reason_notes_are_bounded_and_never_read_through_a_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().join("wal")).await.expect("open");
        let deferred = dir.path().join("wal").join(DEFERRED_DIR);
        fs::create_dir_all(&deferred).await.expect("deferred dir");
        let secret = dir.path().join("secret.txt");
        fs::write(&secret, "must not leak").await.expect("secret");

        fs::write(deferred.join("a.b.sealed-wal"), b"x")
            .await
            .expect("generation");
        fs::write(deferred.join("a.b.sealed-wal.reason"), "y".repeat(10_000))
            .await
            .expect("huge note");
        fs::write(deferred.join("c.b.sealed-wal"), b"x")
            .await
            .expect("generation");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, deferred.join("c.b.sealed-wal.reason"))
            .expect("planted symlink");

        let listed = wal_dir.deferred().await.expect("listing still succeeds");
        assert_eq!(listed.len(), 2, "both generations are still reported");
        let huge = listed
            .iter()
            .find(|g| g.path.ends_with("a.b.sealed-wal"))
            .unwrap();
        let reason = huge.reason.as_deref().unwrap();
        assert!(reason.chars().count() <= MAX_REASON_BYTES as usize + 1);
        assert!(reason.ends_with('…'));
        #[cfg(unix)]
        {
            let linked = listed
                .iter()
                .find(|g| g.path.ends_with("c.b.sealed-wal"))
                .unwrap();
            assert!(
                linked.reason.is_none(),
                "a symlinked note is ignored, not followed"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_deferred_directory_is_not_listed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().join("wal")).await.expect("open");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).await.expect("elsewhere");
        fs::write(elsewhere.join("z.b.sealed-wal"), b"x")
            .await
            .expect("file");
        std::os::unix::fs::symlink(&elsewhere, dir.path().join("wal").join(DEFERRED_DIR))
            .expect("planted symlink");
        assert!(wal_dir.deferred().await.expect("listing").is_empty());
    }

    #[tokio::test]
    async fn remove_deletes_wal_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");

        let mut stream = wal_dir.stream("container-h").await.expect("stream");
        stream
            .append(&sample_line("container-h", "x"))
            .await
            .expect("append");
        stream.sync().await.expect("sync");
        drop(stream);

        wal_dir.remove("container-h").await.expect("remove");
        // Removing again is a no-op, not an error.
        wal_dir
            .remove("container-h")
            .await
            .expect("remove idempotent");

        let recovered = wal_dir.recover().await.expect("recover");
        assert!(recovered.is_empty());
    }

    #[test]
    fn sanitise_replaces_unsafe_chars() {
        assert_eq!(sanitise("abc123"), "abc123");
        assert_eq!(sanitise("abc/def"), "abc_def");
        assert_eq!(sanitise("a.b:c"), "a_b_c");
    }
}
