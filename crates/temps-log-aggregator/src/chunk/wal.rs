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

use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tracing::warn;

use crate::error::LogAggregatorError;
use crate::types::LogLine;

const RECORD_HEADER_LEN: usize = 8; // u32 len + u32 crc32
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

/// A container's lines recovered from its WAL file after a restart.
#[derive(Debug, Clone)]
pub struct RecoveredStream {
    pub container_id: String,
    pub lines: Vec<LogLine>,
    pub generation: Option<WalGeneration>,
}

/// Immutable WAL input for exactly one seal. The bloom choice is persisted
/// in its filename so replay produces the same encoded bytes and storage key.
#[derive(Debug, Clone)]
pub struct WalGeneration {
    path: PathBuf,
    pub with_bloom: bool,
    remove_after_commit: bool,
    recovery_identity: Option<String>,
}

impl WalGeneration {
    pub async fn remove(&self) -> Result<(), LogAggregatorError> {
        if !self.remove_after_commit {
            return Ok(());
        }
        match fs::remove_file(&self.path).await {
            Ok(()) => sync_parent(&self.path).await,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn recovery_identity(&self) -> Option<&str> {
        self.recovery_identity.as_deref()
    }
}

impl WalRecovery {
    fn new(paths: Vec<(PathBuf, bool, bool)>, batch_bytes: usize) -> Self {
        Self {
            paths: paths.into_iter().rev().collect(),
            batch_bytes: batch_bytes.max(1),
            current: None,
        }
    }

    pub async fn next(&mut self) -> Result<Option<RecoveredStream>, LogAggregatorError> {
        loop {
            if self.current.is_none() {
                let Some((path, with_bloom, batchable)) = self.paths.pop() else {
                    return Ok(None);
                };
                let file_len = fs::metadata(&path).await?.len();
                let whole_generation_limit = self.batch_bytes.saturating_add(MAX_RECORD_BYTES);
                if !batchable && file_len > whole_generation_limit as u64 {
                    warn!(
                        path = %path.display(),
                        file_bytes = file_len,
                        recovery_limit_bytes = whole_generation_limit,
                        action = "retained for manual recovery",
                        "wal: immutable generation exceeds safe whole-generation recovery limit; deferring without blocking other generations"
                    );
                    continue;
                }
                self.current = Some(RecoveryFile {
                    reader: BufReader::new(File::open(&path).await?),
                    generation: WalGeneration {
                        path,
                        with_bloom,
                        remove_after_commit: false,
                        recovery_identity: None,
                    },
                    offset: 0,
                    pending_header: None,
                    batchable,
                    tail_corrupt: false,
                });
            }

            let current = self
                .current
                .as_mut()
                .ok_or_else(|| LogAggregatorError::Validation {
                    message: "WAL recovery cursor lost its current file".to_string(),
                })?;
            let mut lines = Vec::new();
            let mut payload_bytes = 0usize;
            let mut batch_start = None;
            let (reached_end, clean_end) = loop {
                let (len, crc, record_offset) = match current.pending_header.take() {
                    Some(header) => header,
                    None => match read_record_header(current).await? {
                        Some(header) => header,
                        None => break (true, !current.tail_corrupt),
                    },
                };
                if len > MAX_RECORD_BYTES {
                    warn!(path = %current.generation.path.display(), offset = record_offset, len, max_record_bytes = MAX_RECORD_BYTES, "wal: record exceeds recovery limit; retaining source and continuing with other files");
                    break (true, false);
                }
                if current.batchable
                    && !lines.is_empty()
                    && payload_bytes.saturating_add(len) > self.batch_bytes
                {
                    current.pending_header = Some((len, crc, record_offset));
                    break (false, false);
                }
                batch_start.get_or_insert(record_offset);
                let mut payload = vec![0; len];
                if let Err(error) = current.reader.read_exact(&mut payload).await {
                    if error.kind() == std::io::ErrorKind::UnexpectedEof {
                        warn!(path = %current.generation.path.display(), offset = record_offset, error = %error, "wal: truncated record payload, stopping recovery for this file");
                        break (true, false);
                    }
                    return Err(wal_read_error(current, record_offset, error));
                }
                current.offset += len as u64;
                if crc32fast::hash(&payload) != crc {
                    warn!(path = %current.generation.path.display(), offset = record_offset, "wal: crc mismatch, stopping recovery for this file");
                    break (true, false);
                }
                match serde_json::from_slice::<LogLine>(&payload) {
                    Ok(line) => lines.push(line),
                    Err(error) => {
                        warn!(path = %current.generation.path.display(), offset = record_offset, error = %error, "wal: failed to deserialize record, stopping recovery for this file");
                        break (true, false);
                    }
                }
                payload_bytes = payload_bytes.saturating_add(len);
            };

            if lines.is_empty() {
                if clean_end {
                    let mut empty_generation = current.generation.clone();
                    empty_generation.remove_after_commit = true;
                    empty_generation.remove().await?;
                }
                self.current = None;
                continue;
            }
            let container_id = lines[0].container_id.clone();
            let mut generation = current.generation.clone();
            // A corrupt/truncated tail is never deleted automatically. Its
            // valid prefix can commit idempotently, while the source remains
            // available for diagnosis or repair.
            generation.remove_after_commit = clean_end;
            if current.batchable {
                let start = batch_start.ok_or_else(|| LogAggregatorError::Validation {
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
            if reached_end {
                self.current = None;
            }
            return Ok(Some(RecoveredStream {
                container_id,
                lines,
                generation: Some(generation),
            }));
        }
    }
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
            warn!(path = %current.generation.path.display(), offset = record_offset, error = %error, "wal: truncated record header, stopping recovery for this file");
            current.tail_corrupt = true;
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

/// Bounded, file-at-a-time WAL recovery cursor.
pub struct WalRecovery {
    paths: Vec<(PathBuf, bool, bool)>,
    batch_bytes: usize,
    current: Option<RecoveryFile>,
}

struct RecoveryFile {
    reader: BufReader<File>,
    generation: WalGeneration,
    offset: u64,
    pending_header: Option<(usize, u32, u64)>,
    batchable: bool,
    tail_corrupt: bool,
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
        remove_after_commit: true,
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
                return Ok(WalRecovery::new(paths, batch_bytes))
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
        Ok(WalRecovery::new(paths, batch_bytes))
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
                remove_after_commit: true,
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

    #[tokio::test]
    async fn oversized_immutable_generation_is_retained_without_splitting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wal_dir = WalDir::open(dir.path().to_path_buf()).await.expect("open");
        let mut stream = wal_dir
            .stream("oversized-generation")
            .await
            .expect("stream");
        for _ in 0..2 {
            stream
                .append(&sample_line(
                    "oversized-generation",
                    &"x".repeat(9 * 1024 * 1024),
                ))
                .await
                .expect("append");
        }
        let generation = stream.rotate(true).await.expect("rotate");
        let generation_path = generation.path.clone();
        stream
            .append(&sample_line("oversized-generation", "small active tail"))
            .await
            .expect("append tail");
        stream.sync().await.expect("sync tail");
        drop(stream);

        let mut recovery = wal_dir
            .recover_batched(1, true)
            .await
            .expect("recovery cursor");
        let recovered = recovery
            .next()
            .await
            .expect("recovery")
            .expect("small generation after deferred oversized generation");
        assert_eq!(recovered.lines[0].msg, "small active tail");
        recovered
            .generation
            .expect("generation")
            .remove()
            .await
            .expect("remove small generation");
        assert!(recovery.next().await.expect("recovery end").is_none());
        assert!(generation_path.exists());
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
