//! Domain types for the OTel subsystem.
//!
//! These types are the internal representation of OTel data after it has been
//! extracted from protobuf payloads. The storage layer works with these types,
//! making it independent of the wire format.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

/// Resource attributes extracted from OTel resource descriptors.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceInfo {
    pub service_name: String,
    pub service_version: Option<String>,
    pub deployment_environment: Option<String>,
    #[schema(value_type = Object)]
    pub attributes: BTreeMap<String, AttributeValue>,
}

impl Default for ResourceInfo {
    fn default() -> Self {
        Self {
            service_name: "unknown".to_string(),
            service_version: None,
            deployment_environment: None,
            attributes: BTreeMap::new(),
        }
    }
}

/// A typed attribute value matching OTel's AnyValue.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    String(String),
    Bool(bool),
    Int(i64),
    Double(f64),
    Bytes(Vec<u8>),
    Array(Vec<AttributeValue>),
    Map(BTreeMap<String, AttributeValue>),
}

impl std::fmt::Display for AttributeValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttributeValue::String(s) => write!(f, "{}", s),
            AttributeValue::Bool(b) => write!(f, "{}", b),
            AttributeValue::Int(i) => write!(f, "{}", i),
            AttributeValue::Double(d) => write!(f, "{}", d),
            AttributeValue::Bytes(b) => write!(f, "<{} bytes>", b.len()),
            AttributeValue::Array(a) => write!(f, "[{} items]", a.len()),
            AttributeValue::Map(m) => write!(f, "{{{} entries}}", m.len()),
        }
    }
}

// ── Metrics ──────────────────────────────────────────────────────────

/// The type of an OTel metric.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricType {
    Gauge,
    Sum,
    Histogram,
}

impl std::fmt::Display for MetricType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetricType::Gauge => write!(f, "gauge"),
            MetricType::Sum => write!(f, "sum"),
            MetricType::Histogram => write!(f, "histogram"),
        }
    }
}

/// A single metric data point ready for storage.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MetricPoint {
    pub project_id: i32,
    pub deployment_id: Option<i32>,
    pub resource: ResourceInfo,
    pub metric_name: String,
    pub metric_type: MetricType,
    pub unit: String,
    #[schema(value_type = String, format = DateTime)]
    pub timestamp: DateTime<Utc>,
    /// For Gauge/Sum: the scalar value.
    pub value: Option<f64>,
    /// For Histogram: count of observations.
    pub histogram_count: Option<u64>,
    /// For Histogram: sum of observations.
    pub histogram_sum: Option<f64>,
    /// For Histogram: min value.
    pub histogram_min: Option<f64>,
    /// For Histogram: max value.
    pub histogram_max: Option<f64>,
    /// For Histogram: explicit bucket boundaries.
    pub histogram_bounds: Option<Vec<f64>>,
    /// For Histogram: count per bucket.
    pub histogram_bucket_counts: Option<Vec<u64>>,
    /// Attribute labels on this data point.
    pub attributes: BTreeMap<String, String>,
}

// ── Traces ───────────────────────────────────────────────────────────

/// Span status code.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpanStatusCode {
    Unset,
    Ok,
    Error,
}

impl std::fmt::Display for SpanStatusCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpanStatusCode::Unset => write!(f, "UNSET"),
            SpanStatusCode::Ok => write!(f, "OK"),
            SpanStatusCode::Error => write!(f, "ERROR"),
        }
    }
}

/// Span kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpanKind {
    Unspecified,
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl std::fmt::Display for SpanKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpanKind::Unspecified => write!(f, "UNSPECIFIED"),
            SpanKind::Internal => write!(f, "INTERNAL"),
            SpanKind::Server => write!(f, "SERVER"),
            SpanKind::Client => write!(f, "CLIENT"),
            SpanKind::Producer => write!(f, "PRODUCER"),
            SpanKind::Consumer => write!(f, "CONSUMER"),
        }
    }
}

/// A span event (log-like annotation on a span).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SpanEvent {
    #[schema(value_type = String, format = DateTime)]
    pub timestamp: DateTime<Utc>,
    pub name: String,
    pub attributes: BTreeMap<String, String>,
}

/// A single trace span ready for storage.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SpanRecord {
    pub project_id: i32,
    pub deployment_id: Option<i32>,
    pub resource: ResourceInfo,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: SpanKind,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
    pub duration_ms: f64,
    pub status_code: SpanStatusCode,
    pub status_message: String,
    pub attributes: BTreeMap<String, String>,
    pub events: Vec<SpanEvent>,
}

/// A trace summary for the list view — one row per trace, aggregated from spans.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraceSummary {
    pub trace_id: String,
    pub root_span_name: String,
    pub service_name: String,
    /// The deployment environment from the root span's resource attributes (e.g. "production").
    pub deployment_environment: Option<String>,
    pub kind: SpanKind,
    pub status_code: SpanStatusCode,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    pub span_count: i64,
    pub error_count: i64,
}

// ── Logs ─────────────────────────────────────────────────────────────

/// Log severity level (simplified from OTel's 24 levels).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LogSeverity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl std::fmt::Display for LogSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogSeverity::Trace => write!(f, "TRACE"),
            LogSeverity::Debug => write!(f, "DEBUG"),
            LogSeverity::Info => write!(f, "INFO"),
            LogSeverity::Warn => write!(f, "WARN"),
            LogSeverity::Error => write!(f, "ERROR"),
            LogSeverity::Fatal => write!(f, "FATAL"),
        }
    }
}

/// A single log record ready for storage.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LogRecord {
    pub project_id: i32,
    pub deployment_id: Option<i32>,
    pub resource: ResourceInfo,
    #[schema(value_type = String, format = DateTime)]
    pub timestamp: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub observed_timestamp: DateTime<Utc>,
    pub severity: LogSeverity,
    pub severity_text: String,
    pub body: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub attributes: BTreeMap<String, String>,
}

// ── Insights / Anomalies ────────────────────────────────────────────

/// Severity of an anomaly insight.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InsightSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for InsightSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InsightSeverity::Low => write!(f, "low"),
            InsightSeverity::Medium => write!(f, "medium"),
            InsightSeverity::High => write!(f, "high"),
            InsightSeverity::Critical => write!(f, "critical"),
        }
    }
}

/// Status of an insight.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InsightStatus {
    Active,
    Resolved,
}

/// An anomaly insight.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Insight {
    pub id: i64,
    pub project_id: i32,
    pub environment: Option<String>,
    pub service_name: String,
    pub severity: InsightSeverity,
    pub status: InsightStatus,
    pub title: String,
    pub description: String,
    pub metric_name: Option<String>,
    pub correlated_deploy_id: Option<i32>,
    pub anomaly_ids: Vec<i64>,
    #[schema(value_type = String, format = DateTime)]
    pub started_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub resolved_at: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}

// ── Health Summary ──────────────────────────────────────────────────

/// Pre-computed health summary for a project environment.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthSummary {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub service_name: String,
    pub status: HealthStatus,
    pub uptime_pct: f64,
    pub error_rate: f64,
    pub p95_latency_ms: f64,
    pub cpu_usage_pct: f64,
    pub memory_usage_pct: f64,
    pub last_deploy_id: Option<i32>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_deploy_at: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = DateTime)]
    pub computed_at: DateTime<Utc>,
}

/// Overall health status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Down,
    Unknown,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthStatus::Healthy => write!(f, "healthy"),
            HealthStatus::Degraded => write!(f, "degraded"),
            HealthStatus::Down => write!(f, "down"),
            HealthStatus::Unknown => write!(f, "unknown"),
        }
    }
}

// ── Pipeline Stats ──────────────────────────────────────────────────

/// Internal pipeline statistics for self-observability.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct PipelineStats {
    pub metrics_received: u64,
    pub metrics_stored: u64,
    pub metrics_dropped: u64,
    pub spans_received: u64,
    pub spans_stored: u64,
    pub spans_dropped: u64,
    pub logs_received: u64,
    pub logs_stored_db: u64,
    pub logs_stored_s3: u64,
    pub logs_dropped: u64,
    pub ingest_errors: u64,
}

// ── Query types ─────────────────────────────────────────────────────

/// Filter for querying traces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceQuery {
    pub project_id: i32,
    pub trace_id: Option<String>,
    pub service_name: Option<String>,
    pub status: Option<SpanStatusCode>,
    pub min_duration_ms: Option<f64>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    /// Filter by environment ID (joins with deployments table).
    pub environment_id: Option<i32>,
    /// Filter by deployment ID (direct column on otel_spans).
    pub deployment_id: Option<i32>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Filter for querying metrics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricQuery {
    pub project_id: i32,
    pub metric_name: Option<String>,
    pub service_name: Option<String>,
    pub environment: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub bucket_interval: Option<String>,
    pub limit: Option<u64>,
}

/// Filter for querying log records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogQuery {
    pub project_id: i32,
    pub severity: Option<LogSeverity>,
    pub service_name: Option<String>,
    pub search: Option<String>,
    pub trace_id: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// A time-bucketed metric aggregate for chart display.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MetricBucket {
    #[schema(value_type = String, format = DateTime)]
    pub bucket: DateTime<Utc>,
    pub avg_value: f64,
    pub min_value: f64,
    pub max_value: f64,
    pub count: i64,
}

/// Quota usage information for a project.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StorageQuota {
    pub project_id: i32,
    pub metrics_bytes: u64,
    pub traces_bytes: u64,
    pub logs_bytes: u64,
    pub total_bytes: u64,
    pub limit_bytes: u64,
    pub usage_pct: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── LogSeverity ordering ────────────────────────────────────────

    #[test]
    fn test_log_severity_ordering() {
        assert!(LogSeverity::Trace < LogSeverity::Debug);
        assert!(LogSeverity::Debug < LogSeverity::Info);
        assert!(LogSeverity::Info < LogSeverity::Warn);
        assert!(LogSeverity::Warn < LogSeverity::Error);
        assert!(LogSeverity::Error < LogSeverity::Fatal);
    }

    #[test]
    fn test_log_severity_warn_is_ge_warn() {
        // This ordering is used by OtelService for log routing (WARN+ → DB)
        assert!(LogSeverity::Warn >= LogSeverity::Warn);
        assert!(LogSeverity::Error >= LogSeverity::Warn);
        assert!(LogSeverity::Fatal >= LogSeverity::Warn);
        assert!(LogSeverity::Info < LogSeverity::Warn);
    }

    // ── InsightSeverity ordering ────────────────────────────────────

    #[test]
    fn test_insight_severity_ordering() {
        assert!(InsightSeverity::Low < InsightSeverity::Medium);
        assert!(InsightSeverity::Medium < InsightSeverity::High);
        assert!(InsightSeverity::High < InsightSeverity::Critical);
    }

    // ── Display impls ───────────────────────────────────────────────

    #[test]
    fn test_log_severity_display() {
        assert_eq!(LogSeverity::Trace.to_string(), "TRACE");
        assert_eq!(LogSeverity::Debug.to_string(), "DEBUG");
        assert_eq!(LogSeverity::Info.to_string(), "INFO");
        assert_eq!(LogSeverity::Warn.to_string(), "WARN");
        assert_eq!(LogSeverity::Error.to_string(), "ERROR");
        assert_eq!(LogSeverity::Fatal.to_string(), "FATAL");
    }

    #[test]
    fn test_span_status_code_display() {
        assert_eq!(SpanStatusCode::Unset.to_string(), "UNSET");
        assert_eq!(SpanStatusCode::Ok.to_string(), "OK");
        assert_eq!(SpanStatusCode::Error.to_string(), "ERROR");
    }

    #[test]
    fn test_span_kind_display() {
        assert_eq!(SpanKind::Unspecified.to_string(), "UNSPECIFIED");
        assert_eq!(SpanKind::Internal.to_string(), "INTERNAL");
        assert_eq!(SpanKind::Server.to_string(), "SERVER");
        assert_eq!(SpanKind::Client.to_string(), "CLIENT");
        assert_eq!(SpanKind::Producer.to_string(), "PRODUCER");
        assert_eq!(SpanKind::Consumer.to_string(), "CONSUMER");
    }

    #[test]
    fn test_metric_type_display() {
        assert_eq!(MetricType::Gauge.to_string(), "gauge");
        assert_eq!(MetricType::Sum.to_string(), "sum");
        assert_eq!(MetricType::Histogram.to_string(), "histogram");
    }

    #[test]
    fn test_health_status_display() {
        assert_eq!(HealthStatus::Healthy.to_string(), "healthy");
        assert_eq!(HealthStatus::Degraded.to_string(), "degraded");
        assert_eq!(HealthStatus::Down.to_string(), "down");
        assert_eq!(HealthStatus::Unknown.to_string(), "unknown");
    }

    #[test]
    fn test_insight_severity_display() {
        assert_eq!(InsightSeverity::Low.to_string(), "low");
        assert_eq!(InsightSeverity::Medium.to_string(), "medium");
        assert_eq!(InsightSeverity::High.to_string(), "high");
        assert_eq!(InsightSeverity::Critical.to_string(), "critical");
    }

    // ── AttributeValue Display ──────────────────────────────────────

    #[test]
    fn test_attribute_value_display() {
        assert_eq!(AttributeValue::String("hello".into()).to_string(), "hello");
        assert_eq!(AttributeValue::Bool(true).to_string(), "true");
        assert_eq!(AttributeValue::Int(42).to_string(), "42");
        assert_eq!(AttributeValue::Double(3.15).to_string(), "3.15");
        assert_eq!(
            AttributeValue::Bytes(vec![1, 2, 3]).to_string(),
            "<3 bytes>"
        );
        assert_eq!(
            AttributeValue::Array(vec![AttributeValue::Int(1), AttributeValue::Int(2)]).to_string(),
            "[2 items]"
        );
        assert_eq!(
            AttributeValue::Map(BTreeMap::from([("k".into(), AttributeValue::Int(1))])).to_string(),
            "{1 entries}"
        );
    }

    // ── Default impls ───────────────────────────────────────────────

    #[test]
    fn test_resource_info_default() {
        let r = ResourceInfo::default();
        assert_eq!(r.service_name, "unknown");
        assert!(r.service_version.is_none());
        assert!(r.deployment_environment.is_none());
        assert!(r.attributes.is_empty());
    }

    #[test]
    fn test_pipeline_stats_default() {
        let s = PipelineStats::default();
        assert_eq!(s.metrics_received, 0);
        assert_eq!(s.spans_received, 0);
        assert_eq!(s.logs_received, 0);
        assert_eq!(s.ingest_errors, 0);
    }

    // ── Serde roundtrips ────────────────────────────────────────────

    #[test]
    fn test_log_severity_serde_roundtrip() {
        let json = serde_json::to_string(&LogSeverity::Error).unwrap();
        assert_eq!(json, "\"ERROR\"");
        let parsed: LogSeverity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, LogSeverity::Error);
    }

    #[test]
    fn test_span_status_code_serde_roundtrip() {
        let json = serde_json::to_string(&SpanStatusCode::Ok).unwrap();
        assert_eq!(json, "\"OK\"");
        let parsed: SpanStatusCode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SpanStatusCode::Ok);
    }

    #[test]
    fn test_metric_type_serde_roundtrip() {
        let json = serde_json::to_string(&MetricType::Histogram).unwrap();
        assert_eq!(json, "\"histogram\"");
        let parsed: MetricType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, MetricType::Histogram);
    }

    #[test]
    fn test_insight_severity_serde_roundtrip() {
        let json = serde_json::to_string(&InsightSeverity::Critical).unwrap();
        assert_eq!(json, "\"critical\"");
        let parsed: InsightSeverity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, InsightSeverity::Critical);
    }

    #[test]
    fn test_health_status_serde_roundtrip() {
        let json = serde_json::to_string(&HealthStatus::Degraded).unwrap();
        assert_eq!(json, "\"degraded\"");
        let parsed: HealthStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, HealthStatus::Degraded);
    }

    #[test]
    fn test_attribute_value_serde_roundtrip() {
        let val = AttributeValue::String("test".into());
        let json = serde_json::to_string(&val).unwrap();
        let parsed: AttributeValue = serde_json::from_str(&json).unwrap();
        match parsed {
            AttributeValue::String(s) => assert_eq!(s, "test"),
            _ => panic!("Expected String variant"),
        }
    }
}
