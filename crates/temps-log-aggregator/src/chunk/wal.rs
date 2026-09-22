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
//! stream's WAL is truncated — never before, so a crash between object write
//! and manifest commit still recovers from the WAL.

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
            writer: BufWriter::new(file),
            bytes_written,
        })
    }

    /// Replay every `*.wal` file in the directory that has at least one
    /// valid record. Truncated or corrupt tail records are skipped with a
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
            if path.extension().and_then(|e| e.to_str()) != Some("wal") {
                continue;
            }
            let lines = read_records(&path).await?;
            if lines.is_empty() {
                continue;
            }
            let container_id = lines[0].container_id.clone();
            out.push(RecoveredStream {
                container_id,
                lines,
            });
        }
        Ok(out)
    }

    /// Delete the WAL file for `container_id`, if any.
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
    #[cfg_attr(not(test), allow(dead_code))]
    path: PathBuf,
    writer: BufWriter<File>,
    bytes_written: u64,
}

impl StreamWal {
    #[cfg(test)]
    pub(crate) async fn read_only_for_test(path: PathBuf) -> Self {
        Self {
            writer: BufWriter::new(File::open(&path).await.unwrap()),
            path,
            bytes_written: 0,
        }
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

        self.writer.write_all(&header).await?;
        self.writer.write_all(&payload).await?;
        self.bytes_written += (RECORD_HEADER_LEN + payload.len()) as u64;
        Ok(())
    }

    /// Flush the buffered writer and fsync the underlying file.
    pub async fn sync(&mut self) -> Result<(), LogAggregatorError> {
        self.writer.flush().await?;
        self.writer.get_ref().sync_data().await?;
        Ok(())
    }

    /// Truncate the WAL back to empty. Callers must only do this after the
    /// chunk sealed from this stream's buffered lines has both been written
    /// to object storage *and* had its manifest row committed — otherwise a
    /// crash between those two steps loses the lines this WAL was protecting.
    pub async fn truncate(&mut self) -> Result<(), LogAggregatorError> {
        self.writer.flush().await?;
        self.writer.get_ref().set_len(0).await?;
        self.writer.get_ref().sync_data().await?;
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
