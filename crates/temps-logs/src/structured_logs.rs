//! Structured logging with JSONL format
//!
//! This module provides structured logging capabilities using JSONL (JSON Lines) format.
//! Each log entry is a complete JSON object on a single line, making it easy to:
//! - Parse and search logs programmatically
//! - Filter by log level
//! - Display with rich formatting (icons, colors)
//! - Stream logs in real-time
//!
//! ## Log Entry Format
//!
//! ```json
//! {
//!   "level": "info",
//!   "message": "Container is running",
//!   "timestamp": "2024-01-20T10:23:52.789Z",
//!   "line": 6,
//!   "metadata": {"container_id": "abc123"}
//! }
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Log level for structured log entries
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

// Note: Icon/color rendering is handled by the frontend
// The frontend will map LogLevel to appropriate UI elements

/// A structured log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Log level (info, success, warning, error)
    pub level: LogLevel,

    /// Log message
    pub message: String,

    /// Timestamp in ISO 8601 format (UTC)
    pub timestamp: DateTime<Utc>,

    /// Line number in the log file
    pub line: u64,

    /// Optional metadata (key-value pairs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl LogEntry {
    /// Create a new log entry
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
            timestamp: Utc::now(),
            line: 0, // Will be set when appending
            metadata: None,
        }
    }

    /// Add metadata to the log entry
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Serialize to JSONL format (single line JSON)
    pub fn to_jsonl(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string(self)?;
        json.push('\n');
        Ok(json)
    }

    /// Parse from JSONL format
    pub fn from_jsonl(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

/// Structured log service using JSONL format
pub struct StructuredLogService {
    log_base_path: PathBuf,
}

impl StructuredLogService {
    pub fn new(log_base_path: PathBuf) -> Self {
        Self { log_base_path }
    }

    /// Get full path for a log file
    pub fn get_log_path(&self, log_id: &str) -> PathBuf {
        if log_id.contains('/') || log_id.ends_with(".jsonl") {
            self.log_base_path.join(log_id)
        } else {
            self.log_base_path.join(format!("{}.jsonl", log_id))
        }
    }

    /// Append a log entry to a JSONL file
    pub async fn append_log(
        &self,
        log_id: &str,
        mut entry: LogEntry,
    ) -> Result<(), std::io::Error> {
        let log_path = self.get_log_path(log_id);

        // Ensure parent directory exists
        if let Some(parent) = log_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Get current line count
        let line_count = if log_path.exists() {
            self.count_lines(log_id).await?
        } else {
            0
        };

        // Set line number
        entry.line = line_count + 1;

        // Serialize and append
        let json_line = entry
            .to_jsonl()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .await?;

        file.write_all(json_line.as_bytes()).await?;
        file.flush().await?;

        Ok(())
    }

    /// Read all log entries from a JSONL file
    pub async fn read_logs(&self, log_id: &str) -> Result<Vec<LogEntry>, std::io::Error> {
        let log_path = self.get_log_path(log_id);

        if !log_path.exists() {
            return Ok(Vec::new());
        }

        let file = tokio::fs::File::open(log_path).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut entries = Vec::new();

        while let Some(line) = lines.next_line().await? {
            if let Ok(entry) = LogEntry::from_jsonl(&line) {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    /// Search logs by text (case-insensitive)
    pub async fn search_logs(
        &self,
        log_id: &str,
        query: &str,
    ) -> Result<Vec<LogEntry>, std::io::Error> {
        let all_logs = self.read_logs(log_id).await?;
        let query_lower = query.to_lowercase();

        Ok(all_logs
            .into_iter()
            .filter(|entry| entry.message.to_lowercase().contains(&query_lower))
            .collect())
    }

    /// Filter logs by level
    pub async fn filter_by_level(
        &self,
        log_id: &str,
        level: LogLevel,
    ) -> Result<Vec<LogEntry>, std::io::Error> {
        let all_logs = self.read_logs(log_id).await?;

        Ok(all_logs
            .into_iter()
            .filter(|entry| entry.level == level)
            .collect())
    }

    /// Count total lines in a log file
    async fn count_lines(&self, log_id: &str) -> Result<u64, std::io::Error> {
        let log_path = self.get_log_path(log_id);

        if !log_path.exists() {
            return Ok(0);
        }

        let file = tokio::fs::File::open(log_path).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut count = 0;

        while lines.next_line().await?.is_some() {
            count += 1;
        }

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_log_entry_serialization() {
        let entry = LogEntry::new(LogLevel::Info, "Test message");
        let jsonl = entry.to_jsonl().unwrap();

        // Should end with newline
        assert!(jsonl.ends_with('\n'));

        // Should be valid JSON
        let parsed = LogEntry::from_jsonl(&jsonl).unwrap();
        assert_eq!(parsed.message, "Test message");
        assert_eq!(parsed.level, LogLevel::Info);
    }

    #[tokio::test]
    async fn test_log_levels() {
        // Log levels serialize correctly to JSON
        let info_json = serde_json::to_string(&LogLevel::Info).unwrap();
        let success_json = serde_json::to_string(&LogLevel::Success).unwrap();
        let warning_json = serde_json::to_string(&LogLevel::Warning).unwrap();
        let error_json = serde_json::to_string(&LogLevel::Error).unwrap();

        assert_eq!(info_json, r#""info""#);
        assert_eq!(success_json, r#""success""#);
        assert_eq!(warning_json, r#""warning""#);
        assert_eq!(error_json, r#""error""#);
    }

    #[tokio::test]
    async fn test_append_and_read_logs() {
        let temp_dir = TempDir::new().unwrap();
        let service = StructuredLogService::new(temp_dir.path().to_path_buf());

        // Append multiple log entries
        service
            .append_log("test", LogEntry::new(LogLevel::Info, "Starting deployment"))
            .await
            .unwrap();

        service
            .append_log(
                "test",
                LogEntry::new(LogLevel::Success, "Container created"),
            )
            .await
            .unwrap();

        service
            .append_log(
                "test",
                LogEntry::new(LogLevel::Warning, "Waiting for ready"),
            )
            .await
            .unwrap();

        service
            .append_log("test", LogEntry::new(LogLevel::Error, "Connection failed"))
            .await
            .unwrap();

        // Read all logs
        let logs = service.read_logs("test").await.unwrap();

        assert_eq!(logs.len(), 4);
        assert_eq!(logs[0].line, 1);
        assert_eq!(logs[0].message, "Starting deployment");
        assert_eq!(logs[1].line, 2);
        assert_eq!(logs[2].line, 3);
        assert_eq!(logs[3].line, 4);
        assert_eq!(logs[3].level, LogLevel::Error);
    }

    #[tokio::test]
    async fn test_search_logs_by_text() {
        let temp_dir = TempDir::new().unwrap();
        let service = StructuredLogService::new(temp_dir.path().to_path_buf());

        // Create test logs
        service
            .append_log(
                "search_test",
                LogEntry::new(LogLevel::Info, "Deploying container image"),
            )
            .await
            .unwrap();

        service
            .append_log(
                "search_test",
                LogEntry::new(LogLevel::Success, "Container is running"),
            )
            .await
            .unwrap();

        service
            .append_log(
                "search_test",
                LogEntry::new(LogLevel::Error, "Container failed to start"),
            )
            .await
            .unwrap();

        // Search for "container" (case-insensitive)
        let results = service
            .search_logs("search_test", "container")
            .await
            .unwrap();

        assert_eq!(results.len(), 3); // All 3 contain "container"

        // Search for "running"
        let results = service.search_logs("search_test", "running").await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message, "Container is running");

        // Search for "failed"
        let results = service.search_logs("search_test", "failed").await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].level, LogLevel::Error);

        // Case insensitive search
        let results = service
            .search_logs("search_test", "DEPLOYING")
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message, "Deploying container image");
    }

    #[tokio::test]
    async fn test_filter_logs_by_level() {
        let temp_dir = TempDir::new().unwrap();
        let service = StructuredLogService::new(temp_dir.path().to_path_buf());

        // Create mixed level logs
        service
            .append_log(
                "filter_test",
                LogEntry::new(LogLevel::Info, "Info message 1"),
            )
            .await
            .unwrap();

        service
            .append_log(
                "filter_test",
                LogEntry::new(LogLevel::Success, "Success message"),
            )
            .await
            .unwrap();

        service
            .append_log(
                "filter_test",
                LogEntry::new(LogLevel::Info, "Info message 2"),
            )
            .await
            .unwrap();

        service
            .append_log(
                "filter_test",
                LogEntry::new(LogLevel::Error, "Error message"),
            )
            .await
            .unwrap();

        service
            .append_log(
                "filter_test",
                LogEntry::new(LogLevel::Warning, "Warning message"),
            )
            .await
            .unwrap();

        // Filter by Info
        let info_logs = service
            .filter_by_level("filter_test", LogLevel::Info)
            .await
            .unwrap();
        assert_eq!(info_logs.len(), 2);

        // Filter by Success
        let success_logs = service
            .filter_by_level("filter_test", LogLevel::Success)
            .await
            .unwrap();
        assert_eq!(success_logs.len(), 1);

        // Filter by Error
        let error_logs = service
            .filter_by_level("filter_test", LogLevel::Error)
            .await
            .unwrap();
        assert_eq!(error_logs.len(), 1);

        // Filter by Warning
        let warning_logs = service
            .filter_by_level("filter_test", LogLevel::Warning)
            .await
            .unwrap();
        assert_eq!(warning_logs.len(), 1);
    }

    #[tokio::test]
    async fn test_log_entry_with_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let service = StructuredLogService::new(temp_dir.path().to_path_buf());

        let metadata = serde_json::json!({
            "container_id": "abc123",
            "port": 3000,
            "status": "healthy"
        });

        let entry =
            LogEntry::new(LogLevel::Info, "Container deployed").with_metadata(metadata.clone());

        service.append_log("metadata_test", entry).await.unwrap();

        let logs = service.read_logs("metadata_test").await.unwrap();

        assert_eq!(logs.len(), 1);
        assert!(logs[0].metadata.is_some());

        let stored_metadata = logs[0].metadata.as_ref().unwrap();
        assert_eq!(stored_metadata["container_id"], "abc123");
        assert_eq!(stored_metadata["port"], 3000);
    }

    #[tokio::test]
    async fn test_complex_search_scenario() {
        let temp_dir = TempDir::new().unwrap();
        let service = StructuredLogService::new(temp_dir.path().to_path_buf());

        // Simulate the deployment log from your screenshot
        service
            .append_log(
                "deploy",
                LogEntry::new(LogLevel::Info, "Deploying container image..."),
            )
            .await
            .unwrap();

        service
            .append_log(
                "deploy",
                LogEntry::new(
                    LogLevel::Info,
                    "Detected EXPOSE directive in image: port 3000",
                ),
            )
            .await
            .unwrap();

        service
            .append_log(
                "deploy",
                LogEntry::new(
                    LogLevel::Info,
                    "Detected EXPOSE directive in container: port 3000",
                ),
            )
            .await
            .unwrap();

        service
            .append_log("deploy", LogEntry::new(LogLevel::Success, "Deployment created: 1e6b4d9dc408dd4cd2859be27f4b7e1162ce4ce7b3d1ec99203ec8f4fe71e6cc57"))
            .await
            .unwrap();

        service
            .append_log(
                "deploy",
                LogEntry::new(LogLevel::Warning, "Waiting for container to start..."),
            )
            .await
            .unwrap();

        service
            .append_log(
                "deploy",
                LogEntry::new(LogLevel::Success, "Container is running"),
            )
            .await
            .unwrap();

        service
            .append_log(
                "deploy",
                LogEntry::new(LogLevel::Warning, "Waiting for application to be ready..."),
            )
            .await
            .unwrap();

        service
            .append_log(
                "deploy",
                LogEntry::new(LogLevel::Info, "Health check URL: http://localhost:56521/"),
            )
            .await
            .unwrap();

        service
            .append_log("deploy", LogEntry::new(LogLevel::Error, "Connectivity check failed (error sending request for url (http://localhost:56521/)), retrying..."))
            .await
            .unwrap();

        service
            .append_log(
                "deploy",
                LogEntry::new(
                    LogLevel::Success,
                    "Connectivity check passed - server responding with status 200 OK (1/2)",
                ),
            )
            .await
            .unwrap();

        service
            .append_log(
                "deploy",
                LogEntry::new(
                    LogLevel::Success,
                    "Connectivity check passed - server responding with status 200 OK (2/2)",
                ),
            )
            .await
            .unwrap();

        // Test various searches
        let port_results = service.search_logs("deploy", "port 3000").await.unwrap();
        assert_eq!(port_results.len(), 2); // Two EXPOSE directives

        let error_results = service.search_logs("deploy", "failed").await.unwrap();
        assert_eq!(error_results.len(), 1);
        assert_eq!(error_results[0].level, LogLevel::Error);

        let health_results = service.search_logs("deploy", "health").await.unwrap();
        assert_eq!(health_results.len(), 1);

        let localhost_results = service.search_logs("deploy", "localhost").await.unwrap();
        assert_eq!(localhost_results.len(), 2); // Health check URL + error message

        // Filter by level
        let successes = service
            .filter_by_level("deploy", LogLevel::Success)
            .await
            .unwrap();
        assert_eq!(successes.len(), 4);

        let errors = service
            .filter_by_level("deploy", LogLevel::Error)
            .await
            .unwrap();
        assert_eq!(errors.len(), 1);
    }
}
