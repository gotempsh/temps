//! Core types for the log aggregator

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A single parsed log line ready for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    /// ISO 8601 timestamp with millisecond precision
    pub ts: DateTime<Utc>,
    /// stdout or stderr
    pub stream: LogStream,
    /// Normalized log level
    pub level: LogLevel,
    /// Raw log message string
    pub msg: String,
    /// Extracted key-value pairs from structured log lines
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    /// Docker container ID
    pub container_id: String,
    /// Service name from Docker label
    pub service: String,
    /// Environment (ID as string) from Docker label
    pub env: String,
    /// Platform integer project ID from Docker label
    pub project_id: i32,
    /// Active deployment ID (deployments.id) at time of log
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_id: Option<i32>,
}

/// Log output stream
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
}

impl std::fmt::Display for LogStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogStream::Stdout => write!(f, "stdout"),
            LogStream::Stderr => write!(f, "stderr"),
        }
    }
}

/// Normalized log level
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// Returns true if this level should be indexed in the log_events hypertable
    pub fn is_indexable(&self) -> bool {
        matches!(self, LogLevel::Error | LogLevel::Warn)
    }

    /// Parse a log level from a string, case-insensitive
    pub fn parse(s: &str) -> Option<LogLevel> {
        match s.to_uppercase().as_str() {
            "ERROR" | "ERR" | "FATAL" | "CRITICAL" | "CRIT" => Some(LogLevel::Error),
            "WARN" | "WARNING" => Some(LogLevel::Warn),
            "INFO" | "INFORMATION" => Some(LogLevel::Info),
            "DEBUG" | "DBG" => Some(LogLevel::Debug),
            "TRACE" | "TRC" => Some(LogLevel::Trace),
            _ => None,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Trace => write!(f, "TRACE"),
            LogLevel::Debug => write!(f, "DEBUG"),
            LogLevel::Info => write!(f, "INFO"),
            LogLevel::Warn => write!(f, "WARN"),
            LogLevel::Error => write!(f, "ERROR"),
        }
    }
}

/// Metadata about a stored chunk (mirrors the log_chunks DB row)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMeta {
    pub id: Uuid,
    pub project_id: i32,
    pub env: String,
    pub service: String,
    pub container_id: String,
    pub deploy_id: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub storage_key: String,
    pub line_count: i32,
    pub compressed_size_bytes: i32,
    pub has_errors: bool,
    /// Byte offset of every 100th line (uncompressed) for partial retrieval
    pub line_offsets: Vec<i32>,
}

/// Enrichment context applied to every log line from a container
#[derive(Debug, Clone)]
pub struct ContainerContext {
    pub project_id: i32,
    pub env: String,
    pub service: String,
    pub container_id: String,
    pub deploy_id: Option<i32>,
}

/// Search filter parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSearchFilter {
    pub project_id: i32,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<LogLevel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub envs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Field-level filters: key=value exact match, key>value range
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_filters: Vec<FieldFilter>,
    /// Cursor for pagination
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Page size (default: 100, max: 2000)
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    /// grep -C: number of raw context lines to include before AND after each
    /// match. 0 (default) returns matches only — wire shape unchanged. The
    /// surrounding lines ignore the level/text/field filters (they're raw
    /// neighbors, the whole point of context) and are clamped to
    /// `MAX_CONTEXT_LINES` server-side.
    #[serde(default)]
    pub context_lines: u32,
}

fn default_page_size() -> u32 {
    100
}

/// Upper bound on `context_lines` (each side) to keep response size bounded.
pub const MAX_CONTEXT_LINES: u32 = 50;

/// A single field-level filter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldFilter {
    pub key: String,
    pub op: FieldFilterOp,
    pub value: String,
}

/// Field filter operator
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldFilterOp {
    Eq,
    Gt,
    Lt,
    Gte,
    Lte,
}

/// Search result page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSearchResult {
    pub lines: Vec<LogSearchLine>,
    /// Cursor for the next page, None if no more results
    pub next_cursor: Option<String>,
    /// Whether results came from index or archive scan
    pub search_mode: SearchMode,
    /// How many lines were examined to produce the results
    pub total_scanned: u64,
}

/// A single line in search results
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LogSearchLine {
    #[schema(value_type = String)]
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub service: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    #[schema(value_type = String)]
    pub chunk_id: Uuid,
    pub line_offset: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_id: Option<i32>,
    /// Raw surrounding lines (grep -C). `None` unless `context_lines > 0` was
    /// requested. Overlapping windows between nearby matches are merged: the
    /// shared neighbors appear on the earlier match only, so the frontend can
    /// render one continuous block without duplicated lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<LineContext>,
}

/// Raw surrounding lines for a single match (grep -C style).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LineContext {
    /// Lines immediately before the match, oldest-first.
    pub before: Vec<ContextLine>,
    /// Lines immediately after the match, oldest-first.
    pub after: Vec<ContextLine>,
}

/// Search execution mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Queried from TimescaleDB index (fast)
    Index,
    /// Scanned from S3/filesystem archive (slower)
    Archive,
}

/// Context lines request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRequest {
    pub chunk_id: Uuid,
    pub line_offset: i32,
    /// Number of lines before and after to return (default: 25)
    #[serde(default = "default_context_lines")]
    pub lines: u32,
}

fn default_context_lines() -> u32 {
    25
}

/// Context lines response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub lines: Vec<ContextLine>,
    /// The index of the target line within the returned lines
    pub target_index: usize,
}

/// A line in context response
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContextLine {
    #[schema(value_type = String)]
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    pub line_offset: i32,
    /// Whether this line matched the original search
    pub is_match: bool,
}

/// Live tail filter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailFilter {
    pub project_id: i32,
    pub service: String,
    pub env: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<LogLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Service tag
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceTag {
    pub project_id: i32,
    pub service: String,
    pub key: String,
    pub value: String,
}

/// Storage configuration
#[derive(Debug, Clone)]
pub enum StorageConfig {
    Filesystem {
        base_path: std::path::PathBuf,
    },
    S3 {
        bucket: String,
        prefix: Option<String>,
        region: String,
        endpoint: Option<String>,
        access_key_id: String,
        secret_access_key: String,
        force_path_style: bool,
    },
}

/// Retention configuration per project
#[derive(Debug, Clone)]
pub struct RetentionConfig {
    /// How long to keep raw chunks in S3/filesystem (default: 30 days)
    pub chunk_retention_days: u32,
    /// How long to keep log_events in TimescaleDB (managed by TimescaleDB retention policy: 7 days)
    pub event_retention_days: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            chunk_retention_days: 30,
            event_retention_days: 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_parse() {
        assert_eq!(LogLevel::parse("ERROR"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("error"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("ERR"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("FATAL"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("WARN"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("warning"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse("DEBUG"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse("TRACE"), Some(LogLevel::Trace));
        assert_eq!(LogLevel::parse("unknown"), None);
    }

    #[test]
    fn test_log_level_is_indexable() {
        assert!(LogLevel::Error.is_indexable());
        assert!(LogLevel::Warn.is_indexable());
        assert!(!LogLevel::Info.is_indexable());
        assert!(!LogLevel::Debug.is_indexable());
        assert!(!LogLevel::Trace.is_indexable());
    }

    #[test]
    fn test_log_level_display() {
        assert_eq!(LogLevel::Error.to_string(), "ERROR");
        assert_eq!(LogLevel::Warn.to_string(), "WARN");
        assert_eq!(LogLevel::Info.to_string(), "INFO");
        assert_eq!(LogLevel::Debug.to_string(), "DEBUG");
        assert_eq!(LogLevel::Trace.to_string(), "TRACE");
    }

    #[test]
    fn test_log_stream_display() {
        assert_eq!(LogStream::Stdout.to_string(), "stdout");
        assert_eq!(LogStream::Stderr.to_string(), "stderr");
    }

    #[test]
    fn test_default_retention_config() {
        let config = RetentionConfig::default();
        assert_eq!(config.chunk_retention_days, 30);
        assert_eq!(config.event_retention_days, 7);
    }

    #[test]
    fn test_log_line_serialization() {
        let line = LogLine {
            ts: Utc::now(),
            stream: LogStream::Stdout,
            level: LogLevel::Info,
            msg: "test message".to_string(),
            fields: Some(serde_json::json!({"key": "value"})),
            container_id: "abc123".to_string(),
            service: "web".to_string(),
            env: "2".to_string(),
            project_id: 42,
            deploy_id: None,
        };

        let json = serde_json::to_string(&line).expect("should serialize");
        let parsed: LogLine = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(parsed.msg, "test message");
        assert_eq!(parsed.level, LogLevel::Info);
        assert_eq!(parsed.stream, LogStream::Stdout);
        assert_eq!(parsed.project_id, 42);
    }
}
