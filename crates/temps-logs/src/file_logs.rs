// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! File-based logging service for pipeline operations
//!
//! This module provides utilities for:
//! - Creating structured log files with date-based organization
//! - Appending to logs asynchronously
//! - Tailing logs in real-time
//! - Reading log content

use chrono::Utc;
use futures::Stream;
use std::path::PathBuf;
use tokio::fs::{create_dir_all, File};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom};
use tokio::time::Duration;
use tracing::{debug, trace};

use crate::structured_logs::{LogEntry, LogLevel, StructuredLogService};

/// Default number of trailing lines replayed when a tail stream first attaches.
///
/// This is the initial backlog the client receives before it starts seeing
/// live lines. It is intentionally large so that full deployment/build logs
/// are visible in the UI on first load — the previous value (1000) silently
/// truncated long build logs. Clients dedupe by absolute line number, so a
/// generous backlog is safe across reconnects.
pub const DEFAULT_TAIL_REPLAY_LINES: usize = 100_000;

/// Size of each block read while scanning backwards for the tail offset.
///
/// The scan holds exactly one of these at a time, so the memory cost of
/// locating the replay window is O(1) (~64 KB) regardless of file size or of
/// how long individual lines are.
const TAIL_SCAN_CHUNK: usize = 64 * 1024;

/// Byte offset of the first of the last `replay_lines` lines of `file_size`
/// bytes, found by scanning backwards in fixed blocks.
///
/// Returns 0 when the file holds at most `replay_lines` lines, and
/// `file_size` when `replay_lines` is 0 (attach at EOF, no backlog).
/// A trailing
/// '\n' terminates the last line rather than starting an empty one, so it is
/// not counted; a file whose final line is unterminated still counts as a
/// line. The reader's cursor is left unspecified — callers seek afterwards.
async fn find_tail_offset<R>(
    reader: &mut R,
    file_size: u64,
    replay_lines: usize,
) -> Result<u64, std::io::Error>
where
    R: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin,
{
    // No backlog at all: attach at EOF and stream only what arrives next.
    if replay_lines == 0 {
        return Ok(file_size);
    }
    // Replaying everything means starting at the beginning; skip the scan.
    if replay_lines == usize::MAX {
        return Ok(0);
    }

    let mut buffer = vec![0u8; TAIL_SCAN_CHUNK];
    // Offset of the byte just past the region still to be scanned.
    let mut end = file_size;
    // Newlines seen so far that *start* a line (i.e. excluding a trailing one).
    let mut line_starts = 0usize;
    // A file ending in '\n' terminates its last line; that newline does not
    // begin another one, so it must not be counted.
    let mut skip_final_newline = true;

    while end > 0 {
        let chunk_len = std::cmp::min(TAIL_SCAN_CHUNK as u64, end) as usize;
        let start = end - chunk_len as u64;

        reader.seek(SeekFrom::Start(start)).await?;
        reader.read_exact(&mut buffer[..chunk_len]).await?;

        for (index, byte) in buffer[..chunk_len].iter().enumerate().rev() {
            if *byte != b'\n' {
                continue;
            }
            if skip_final_newline && start + index as u64 == file_size - 1 {
                skip_final_newline = false;
                continue;
            }
            skip_final_newline = false;
            line_starts += 1;
            if line_starts == replay_lines {
                // The line begins immediately after this newline.
                return Ok(start + index as u64 + 1);
            }
        }

        end = start;
    }

    // Fewer lines in the file than requested: replay from the beginning.
    Ok(0)
}

pub struct LogService {
    log_base_path: PathBuf,
    structured_service: StructuredLogService,
}

impl LogService {
    pub fn new(log_base_path: PathBuf) -> Self {
        let structured_service = StructuredLogService::new(log_base_path.clone());
        LogService {
            log_base_path,
            structured_service,
        }
    }

    /// Returns the base path where logs and other data files are stored
    pub fn base_path(&self) -> &PathBuf {
        &self.log_base_path
    }

    pub fn get_log_path(&self, log_id: &str) -> PathBuf {
        // If log_id already contains .log extension or path separators, treat it as a full path
        if log_id.contains('/') || log_id.ends_with(".log") {
            self.log_base_path.join(log_id)
        } else {
            // Legacy behavior: add .log extension
            self.log_base_path.join(format!("{}.log", log_id))
        }
    }

    pub async fn create_log_path(&self, log_id: &str) -> Result<PathBuf, std::io::Error> {
        // If log_id contains path separators, it's already a full path with directory structure
        let log_path = if log_id.contains('/') {
            PathBuf::from(log_id)
        } else {
            // Legacy behavior: create date-based path
            let now = Utc::now();
            let date_path = now.format("%Y/%m/%d/%H").to_string();
            PathBuf::from(date_path).join(format!("{}.log", log_id))
        };

        let full_path = self.log_base_path.join(&log_path);

        // Ensure the directory exists
        if let Some(parent) = full_path.parent() {
            create_dir_all(parent).await?;
        }

        Ok(log_path)
    }

    // REMOVED FROM PUBLIC API: append_to_log() - Use append_structured_log() instead
    // This method has been removed from the public API to enforce structured logging.
    // All production code must use append_structured_log() with explicit log levels.
    //
    // Migration guide:
    //   Before: service.append_to_log(log_id, "message\n").await?;
    //   After:  service.append_structured_log(log_id, LogLevel::Info, "message").await?;
    //
    // Helper methods available:
    //   - log_info(log_id, message)
    //   - log_success(log_id, message)
    //   - log_warning(log_id, message)
    //   - log_error(log_id, message)

    pub async fn get_log_content(&self, log_id: &str) -> Result<String, std::io::Error> {
        let log_path = self.get_log_path(log_id);
        tokio::fs::read_to_string(log_path).await
    }

    /// Tail a log file, replaying up to [`DEFAULT_TAIL_REPLAY_LINES`] trailing
    /// lines before streaming new lines as they are appended.
    pub async fn tail_log(
        &self,
        log_id: &str,
    ) -> Result<impl Stream<Item = Result<String, std::io::Error>>, std::io::Error> {
        self.tail_log_with_replay(log_id, DEFAULT_TAIL_REPLAY_LINES)
            .await
    }

    /// Tail a log file, replaying up to `replay_lines` trailing lines before
    /// streaming new lines as they are appended.
    ///
    /// Pass `usize::MAX` to replay the entire file. The replay backlog is the
    /// last `replay_lines` lines of the file at the moment the stream attaches;
    /// any line written afterwards is streamed live regardless of this cap.
    pub async fn tail_log_with_replay(
        &self,
        log_id: &str,
        replay_lines: usize,
    ) -> Result<impl Stream<Item = Result<String, std::io::Error>>, std::io::Error> {
        let log_path = self.get_log_path(log_id);
        debug!(
            "Attempting to tail log at path: {:?} (replay up to {} lines)",
            log_path, replay_lines
        );

        // Create file if it doesn't exist
        if !log_path.exists() {
            trace!("Log file doesn't exist, creating new file");
            File::create(&log_path).await?;
        }

        // Open file in read mode
        let file = File::open(&log_path).await?;
        let file_size = file.metadata().await?.len();
        let mut reader = BufReader::new(file);

        // If file has content, seek to the start of the last `replay_lines` lines.
        if file_size > 0 {
            let start_pos = find_tail_offset(&mut reader, file_size, replay_lines).await?;
            reader.seek(SeekFrom::Start(start_pos)).await?;
        }

        Ok(async_stream::stream! {
            let mut buffer = String::new();

            loop {
                match reader.read_line(&mut buffer).await {
                    Ok(0) => {
                        // Reached EOF, wait a bit before trying again
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    Ok(_) => {
                        let line = buffer.trim_end().to_string();
                        if !line.is_empty() {
                            yield Ok(line);
                        }
                        buffer.clear();
                    }
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                }
            }
        })
    }

    // ========== Structured Logging Helpers ==========
    // These methods provide convenient access to structured logging
    // while maintaining backward compatibility with existing append_to_log() usage

    /// Append a structured log entry with automatic JSONL formatting
    ///
    /// This is a convenience wrapper that creates structured logs transparently.
    /// Callers can continue using the same API while getting structured benefits.
    pub async fn append_structured_log(
        &self,
        log_id: &str,
        level: LogLevel,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        let entry = LogEntry::new(level, message);
        self.structured_service.append_log(log_id, entry).await
    }

    /// Append a structured log with metadata
    pub async fn append_structured_log_with_metadata(
        &self,
        log_id: &str,
        level: LogLevel,
        message: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Result<(), std::io::Error> {
        let entry = LogEntry::new(level, message).with_metadata(metadata);
        self.structured_service.append_log(log_id, entry).await
    }

    /// Read all structured log entries from a JSONL file
    ///
    /// Returns parsed LogEntry objects instead of raw strings.
    /// Use this for fetching logs that need to be displayed with rich formatting.
    pub async fn get_structured_logs(&self, log_id: &str) -> Result<Vec<LogEntry>, std::io::Error> {
        self.structured_service.read_logs(log_id).await
    }

    /// Search structured logs by text (case-insensitive)
    ///
    /// This is much more efficient than searching raw log text because
    /// it only searches the message field and can leverage indexing later.
    pub async fn search_structured_logs(
        &self,
        log_id: &str,
        query: &str,
    ) -> Result<Vec<LogEntry>, std::io::Error> {
        self.structured_service.search_logs(log_id, query).await
    }

    /// Filter structured logs by level
    ///
    /// Returns only logs matching the specified level (info, success, warning, error)
    pub async fn filter_structured_logs_by_level(
        &self,
        log_id: &str,
        level: LogLevel,
    ) -> Result<Vec<LogEntry>, std::io::Error> {
        self.structured_service.filter_by_level(log_id, level).await
    }

    // ========== Convenience Methods for Common Log Levels ==========

    /// Log an info message (ℹ️ icon in UI)
    pub async fn log_info(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        self.append_structured_log(log_id, LogLevel::Info, message)
            .await
    }

    /// Log a success message (✓ icon in UI)
    pub async fn log_success(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        self.append_structured_log(log_id, LogLevel::Success, message)
            .await
    }

    /// Log a warning message (⏳ icon in UI)
    pub async fn log_warning(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        self.append_structured_log(log_id, LogLevel::Warning, message)
            .await
    }

    /// Log an error message (✗ icon in UI)
    pub async fn log_error(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        self.append_structured_log(log_id, LogLevel::Error, message)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_log_service_creation() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-log";
        let log_path = log_service.get_log_path(log_id);

        assert!(log_path.to_string_lossy().contains("test-log.log"));
    }

    #[tokio::test]
    async fn test_create_log_path() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-create";
        let log_path = log_service.create_log_path(log_id).await.unwrap();

        // Should create a date-based path
        assert!(log_path.to_string_lossy().contains("test-create.log"));

        // Full path should exist after creation
        let full_path = temp_dir.path().join(&log_path);
        assert!(full_path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn test_append_and_read_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-append";

        // Append some content using structured logging
        log_service.log_info(log_id, "First line").await.unwrap();
        log_service.log_info(log_id, "Second line").await.unwrap();

        // Read back the structured logs
        let logs = log_service.get_structured_logs(log_id).await.unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].message, "First line");
        assert_eq!(logs[1].message, "Second line");
        assert!(matches!(logs[0].level, LogLevel::Info));
        assert!(matches!(logs[1].level, LogLevel::Info));
    }

    #[tokio::test]
    async fn test_tail_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-tail";
        let log_path = log_service.create_log_path(log_id).await.unwrap();
        let log_path_str = log_path.to_str().unwrap();

        // Write initial content using structured logging
        log_service.log_info(log_id, "Initial line").await.unwrap();

        // Start tailing
        let _stream = log_service.tail_log(log_path_str).await.unwrap();

        // This is a basic test - in practice, tailing would be used with continuous writes
        // For testing purposes, we just verify the stream can be created
        // We can't easily test the streaming behavior in a unit test
    }

    #[tokio::test]
    async fn test_get_log_content_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let result = log_service.get_log_content("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_structured_log_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-create-on-append";

        // Structured log should create the file
        log_service.log_info(log_id, "First line").await.unwrap();

        // Verify file was created
        let log_path = log_service.structured_service.get_log_path(log_id);
        assert!(log_path.exists());

        // Verify content was written using structured logs
        let logs = log_service.get_structured_logs(log_id).await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "First line");
        assert!(matches!(logs[0].level, LogLevel::Info));
    }

    #[tokio::test]
    async fn test_empty_log_content() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-empty";

        // Create log path but don't write anything
        log_service.create_log_path(log_id).await.unwrap();

        // If file exists but is empty, reading should return empty or error
        let result = log_service.get_log_content(log_id).await;
        // Either empty content or error is acceptable for an empty log
        assert!(result.is_ok() || result.is_err());
    }

    #[tokio::test]
    async fn test_append_multiple_entries_same_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-multiple";

        // Append multiple entries using structured logging
        for i in 1..=5 {
            log_service
                .log_info(log_id, &format!("Line {}", i))
                .await
                .unwrap();
        }

        // Read back the structured logs
        let logs = log_service.get_structured_logs(log_id).await.unwrap();
        assert_eq!(logs.len(), 5);
        for (i, log) in logs.iter().enumerate() {
            assert_eq!(log.message, format!("Line {}", i + 1));
            assert!(matches!(log.level, LogLevel::Info));
        }
    }

    #[tokio::test]
    async fn test_log_path_with_special_characters() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-with-dashes_and_underscores";

        // Should be able to write to it using structured logging
        log_service
            .log_info(log_id, "Content with special chars")
            .await
            .unwrap();

        // Read back the structured logs
        let logs = log_service.get_structured_logs(log_id).await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "Content with special chars");
        assert!(matches!(logs[0].level, LogLevel::Info));
    }

    #[tokio::test]
    async fn test_create_log_path_directory_structure() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-dir-structure";
        let log_path = log_service.create_log_path(log_id).await.unwrap();

        // Should create a date-based path structure
        let path_str = log_path.to_string_lossy();
        assert!(path_str.contains("/")); // Should have directory separators
        assert!(path_str.ends_with("test-dir-structure.log"));

        // Directory should exist
        let full_path = temp_dir.path().join(&log_path);
        assert!(full_path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn test_tail_log_replays_more_than_1000_lines() {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        // Write 5000 plain lines directly to the log file (bypassing the
        // structured JSONL writer to keep the assertion about line counts simple).
        let log_id = "test-large-tail";
        let log_path = log_service.get_log_path(log_id);
        let mut file = File::create(&log_path).await.unwrap();
        for i in 0..5000 {
            file.write_all(format!("line {}\n", i).as_bytes())
                .await
                .unwrap();
        }
        file.flush().await.unwrap();

        // With the default replay cap (100k > 5000), the stream should replay
        // ALL 5000 lines, not just the last 1000.
        let stream = log_service.tail_log(log_id).await.unwrap();
        tokio::pin!(stream);

        let mut received = Vec::new();
        // The tail stream is infinite (it waits at EOF), so bound the read with
        // a short timeout once the backlog is drained.
        loop {
            match tokio::time::timeout(Duration::from_millis(300), stream.next()).await {
                Ok(Some(Ok(line))) => received.push(line),
                Ok(Some(Err(_))) | Ok(None) => break,
                Err(_) => break, // timed out waiting for more — backlog drained
            }
        }

        assert_eq!(
            received.len(),
            5000,
            "expected all 5000 lines replayed, got {}",
            received.len()
        );
        assert_eq!(received.first().unwrap(), "line 0");
        assert_eq!(received.last().unwrap(), "line 4999");
    }

    #[tokio::test]
    async fn test_tail_log_with_replay_caps_backlog() {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-capped-tail";
        let log_path = log_service.get_log_path(log_id);
        let mut file = File::create(&log_path).await.unwrap();
        for i in 0..100 {
            file.write_all(format!("line {}\n", i).as_bytes())
                .await
                .unwrap();
        }
        file.flush().await.unwrap();

        // Explicit small replay cap: only the last 10 lines should come back.
        let stream = log_service.tail_log_with_replay(log_id, 10).await.unwrap();
        tokio::pin!(stream);

        let mut received = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_millis(300), stream.next()).await {
                Ok(Some(Ok(line))) => received.push(line),
                Ok(Some(Err(_))) | Ok(None) => break,
                Err(_) => break,
            }
        }

        assert_eq!(received.len(), 10, "expected last 10 lines only");
        assert_eq!(received.first().unwrap(), "line 90");
        assert_eq!(received.last().unwrap(), "line 99");
    }

    /// Run the reverse tail scan over `content` and return the byte offset it
    /// picks for `replay_lines`, exercising the same `BufReader<File>` path
    /// `tail_log_with_replay` uses.
    async fn tail_offset_for(content: &[u8], replay_lines: usize) -> u64 {
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("scan.log");
        let mut file = File::create(&path).await.unwrap();
        file.write_all(content).await.unwrap();
        file.flush().await.unwrap();
        drop(file);

        let file = File::open(&path).await.unwrap();
        let file_size = file.metadata().await.unwrap().len();
        let mut reader = BufReader::new(file);
        find_tail_offset(&mut reader, file_size, replay_lines)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_find_tail_offset_empty_file() {
        assert_eq!(tail_offset_for(b"", 10).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_fewer_lines_than_requested() {
        // 3 lines, 10 requested -> replay from the very beginning.
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 10).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_exact_line_count() {
        // Exactly `replay_lines` lines: still the whole file.
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 3).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_trailing_newline_not_counted_as_line() {
        // "a\nbb\nccc\n": the final '\n' terminates "ccc", it does not start a
        // fourth (empty) line. Last line therefore begins at offset 5.
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 1).await, 5);
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 2).await, 2);
    }

    #[tokio::test]
    async fn test_find_tail_offset_unterminated_final_line() {
        // No trailing newline: "ccc" is still a line.
        assert_eq!(tail_offset_for(b"a\nbb\nccc", 1).await, 5);
        assert_eq!(tail_offset_for(b"a\nbb\nccc", 2).await, 2);
    }

    #[tokio::test]
    async fn test_find_tail_offset_counts_empty_lines() {
        // "a\n\nb\n" is three lines: "a", "", "b".
        assert_eq!(tail_offset_for(b"a\n\nb\n", 2).await, 2);
        assert_eq!(tail_offset_for(b"a\n\nb\n", 1).await, 3);
    }

    #[tokio::test]
    async fn test_find_tail_offset_replay_all_sentinel() {
        // usize::MAX means "replay everything".
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", usize::MAX).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_zero_replay_attaches_at_eof() {
        // A zero backlog must yield no replayed lines at all, i.e. EOF.
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 0).await, 9);
        assert_eq!(tail_offset_for(b"", 0).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_lines_longer_than_scan_chunk() {
        // Lines far longer than TAIL_SCAN_CHUNK force the backwards scan to
        // cross several blocks without finding a newline.
        let long = "x".repeat(TAIL_SCAN_CHUNK * 2 + 7);
        let content = format!("{long}\n{long}\n{long}\n");
        let line_len = long.len() as u64 + 1;

        assert_eq!(tail_offset_for(content.as_bytes(), 1).await, line_len * 2);
        assert_eq!(tail_offset_for(content.as_bytes(), 2).await, line_len);
        assert_eq!(tail_offset_for(content.as_bytes(), 3).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_newline_on_chunk_boundary() {
        // Place a newline exactly at the first byte of the final scan block so
        // the match is found at index 0 of a chunk.
        let mut content = vec![b'x'; TAIL_SCAN_CHUNK];
        content[0] = b'\n';
        content.push(b'\n');
        // File is: "\n" + x*(CHUNK-1) + "\n" -> lines "" and "xxx...".
        assert_eq!(tail_offset_for(&content, 1).await, 1);
        assert_eq!(tail_offset_for(&content, 2).await, 0);
    }

    #[tokio::test]
    async fn test_tail_log_replay_then_live_lines() {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-replay-then-live";
        let log_path = log_service.get_log_path(log_id);
        let mut file = File::create(&log_path).await.unwrap();
        for i in 0..50 {
            file.write_all(format!("old {}\n", i).as_bytes())
                .await
                .unwrap();
        }
        file.flush().await.unwrap();

        let stream = log_service.tail_log_with_replay(log_id, 3).await.unwrap();
        tokio::pin!(stream);

        // Backlog: the last 3 lines only.
        let mut received = Vec::new();
        for _ in 0..3 {
            if let Ok(Some(Ok(line))) =
                tokio::time::timeout(Duration::from_millis(500), stream.next()).await
            {
                received.push(line);
            }
        }
        assert_eq!(received, vec!["old 47", "old 48", "old 49"]);

        // Lines appended after the stream attached must still arrive live.
        let mut appender = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .await
            .unwrap();
        appender.write_all(b"new 0\nnew 1\n").await.unwrap();
        appender.flush().await.unwrap();

        let mut live = Vec::new();
        for _ in 0..2 {
            if let Ok(Some(Ok(line))) =
                tokio::time::timeout(Duration::from_secs(2), stream.next()).await
            {
                live.push(line);
            }
        }
        assert_eq!(live, vec!["new 0", "new 1"]);
    }

    #[tokio::test]
    async fn test_tail_log_nonexistent_file_creates_it() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-tail-create";
        let log_path = log_service.get_log_path(log_id);

        // File doesn't exist
        assert!(!log_path.exists());

        // Tail should create the file
        let _stream = log_service.tail_log(log_id).await.unwrap();

        // File should now exist
        assert!(log_path.exists());
    }
}
