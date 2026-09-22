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
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tracing::warn;

use crate::error::LogAggregatorError;
use crate::types::LogLine;

const RECORD_HEADER_LEN: usize = 8; // u32 len + u32 crc32

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
}

impl WalGeneration {
    pub async fn remove(&self) -> Result<(), LogAggregatorError> {
        match fs::remove_file(&self.path).await {
            Ok(()) => sync_parent(&self.path).await,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

async fn sync_parent(path: &Path) -> Result<(), LogAggregatorError> {
    if let Some(parent) = path.parent() {
        File::open(parent).await?.sync_all().await?;
    }
    Ok(())
}

/// Directory of per-stream WAL files.
pub struct WalDir {
    root: PathBuf,
}

impl WalDir {
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

    /// Replay each immutable `*.sealed-wal` generation and each active
    /// `*.wal` tail independently when it has at least one valid record. Truncated or corrupt tail records are skipped with a
    /// warning rather than failing the whole recovery.
    pub async fn recover(&self) -> Result<Vec<RecoveredStream>, LogAggregatorError> {
        let mut out = Vec::new();
        let mut entries = match fs::read_dir(&self.root).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let generation = match path.extension().and_then(|e| e.to_str()) {
                Some("wal") => None,
                Some("sealed-wal") => {
                    let with_bloom = match path
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
                    Some(WalGeneration {
                        path: path.clone(),
                        with_bloom,
                    })
                }
                _ => continue,
            };
            let lines = read_records(&path).await?;
            if lines.is_empty() {
                continue;
            }
            let container_id = lines[0].container_id.clone();
            out.push(RecoveredStream {
                container_id,
                lines,
                generation,
            });
        }
        // Frozen generations never share a seal with the active tail. Recover
        // them first so opening a recovered head cannot consume that tail.
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
            self.frozen = Some(WalGeneration { path, with_bloom });
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
        let crc = crc32fast::hash(&payload);
        let len = payload.len() as u32;

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
