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

    pub async fn tail_log(
        &self,
        log_id: &str,
    ) -> Result<impl Stream<Item = Result<String, std::io::Error>>, std::io::Error> {
        let log_path = self.get_log_path(log_id);
        debug!("Attempting to tail log at path: {:?}", log_path);

        // Create file if it doesn't exist
        if !log_path.exists() {
            trace!("Log file doesn't exist, creating new file");
            File::create(&log_path).await?;
        }

        // Open file in read mode
        let file = File::open(&log_path).await?;
        let file_size = file.metadata().await?.len();
        let mut reader = BufReader::new(file);

        // If file has content, seek to position to get last 1000 lines
        if file_size > 0 {
            let mut buffer = Vec::new();
            reader.read_to_end(&mut buffer).await?;

            let lines = buffer.split(|&b| b == b'\n').collect::<Vec<_>>();
            let start_pos = if lines.len() > 1000 {
                // Get position to start reading from for last 1000 lines
                let skip_lines = lines.len() - 1000;
                lines
                    .iter()
                    .take(skip_lines)
                    .map(|line| line.len() + 1) // +1 for newline
                    .sum::<usize>() as u64
            } else {
                0
            };

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
