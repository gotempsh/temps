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
    ExponentialHistogram,
    Summary,
}

impl std::fmt::Display for MetricType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetricType::Gauge => write!(f, "gauge"),
            MetricType::Sum => write!(f, "sum"),
            MetricType::Histogram => write!(f, "histogram"),
            MetricType::ExponentialHistogram => write!(f, "exponential_histogram"),
            MetricType::Summary => write!(f, "summary"),
        }
    }
}

/// The aggregation applied when reducing raw metric points into a time bucket.
///
/// Store-neutral: every storage backend (ClickHouse today, TimescaleDB later)
/// must be able to satisfy this contract. `Quantile(q)` carries the requested
/// quantile in `[0.0, 1.0]` (e.g. `0.95` for p95).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricAggregation {
    /// Arithmetic mean of the scalar value in each bucket. The default.
    #[default]
    Avg,
    /// Sum of the scalar value in each bucket.
    Sum,
    /// Minimum scalar value in each bucket.
    Min,
    /// Maximum scalar value in each bucket.
    Max,
    /// Number of points in each bucket.
    Count,
    /// Per-second rate of change of a cumulative monotonic counter, computed as
    /// `(max - min) / window_seconds` within each bucket. Non-monotonic series
    /// fall back to a simple delta.
    RatePerSec,
    /// A quantile of the scalar value in each bucket. The carried `f64` is the
    /// requested quantile in `[0.0, 1.0]`.
    #[serde(untagged)]
    Quantile(f64),
}

impl std::fmt::Display for MetricAggregation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetricAggregation::Avg => write!(f, "avg"),
            MetricAggregation::Sum => write!(f, "sum"),
            MetricAggregation::Min => write!(f, "min"),
            MetricAggregation::Max => write!(f, "max"),
            MetricAggregation::Count => write!(f, "count"),
            MetricAggregation::RatePerSec => write!(f, "rate_per_sec"),
            MetricAggregation::Quantile(q) => write!(f, "quantile({q})"),
        }
    }
}

impl MetricAggregation {
    /// Parse a query-string aggregation token into a typed aggregation.
    ///
    /// Accepts the keyword forms (`avg`, `sum`, `min`, `max`, `count`, `rate`)
    /// and quantile forms `p50`/`p95`/`p99` and `quantile:0.95` / `q0.95`.
    /// Unknown or out-of-range inputs fall back to [`MetricAggregation::Avg`].
    pub fn parse(s: &str) -> Self {
        let lower = s.trim().to_ascii_lowercase();
        match lower.as_str() {
            "avg" | "average" | "mean" => return MetricAggregation::Avg,
            "sum" | "total" => return MetricAggregation::Sum,
            "min" | "minimum" => return MetricAggregation::Min,
            "max" | "maximum" => return MetricAggregation::Max,
            "count" => return MetricAggregation::Count,
            "rate" | "rate_per_sec" | "ratepersec" => return MetricAggregation::RatePerSec,
            _ => {}
        }
        // pNN shorthand (p50 -> 0.50, p95 -> 0.95, p999 -> 0.999, p99.9 -> 0.999)
        if let Some(rest) = lower.strip_prefix('p') {
            if let Ok(on) = rest.parse::<f64>() {
                let q = if rest.contains('.') {
                    on / 100.0
                } else {
                    on / 10f64.powi(rest.len() as i32)
                };
                if (0.0..=1.0).contains(&q) {
                    return MetricAggregation::Quantile(q);
                }
            }
        }
        // quantile:0.95 / q0.95 / quantile(0.95)
        for prefix in ["quantile:", "quantile(", "q"] {
            if let Some(rest) = lower.strip_prefix(prefix) {
                let cleaned = rest.trim_end_matches(')');
                if let Ok(q) = cleaned.parse::<f64>() {
                    if (0.0..=1.0).contains(&q) {
                        return MetricAggregation::Quantile(q);
                    }
                }
            }
        }
        MetricAggregation::Avg
    }

    /// The clamped quantile for a `Quantile` aggregation, else `None`.
    pub fn quantile(&self) -> Option<f64> {
        match self {
            MetricAggregation::Quantile(q) => Some(q.clamp(0.0, 1.0)),
            _ => None,
        }
    }
}

/// The aggregation temporality of a Sum/Histogram/ExponentialHistogram metric.
///
/// Mirrors OTel's `AggregationTemporality` proto enum: whether reported values
/// are cumulative since the start of the series (Cumulative) or only the delta
/// since the previous report (Delta).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AggregationTemporality {
    /// Temporality not reported by the producer.
    Unspecified,
    /// Each value covers only the interval since the previous report.
    Delta,
    /// Each value is cumulative since the start of the series.
    Cumulative,
}

impl std::fmt::Display for AggregationTemporality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregationTemporality::Unspecified => write!(f, "unspecified"),
            AggregationTemporality::Delta => write!(f, "delta"),
            AggregationTemporality::Cumulative => write!(f, "cumulative"),
        }
    }
}

impl AggregationTemporality {
    /// Map the raw protobuf `aggregation_temporality` i32 to our enum.
    ///
    /// Unknown values fall back to `Unspecified` so unexpected producers never
    /// cause a decode failure.
    pub fn from_proto(value: i32) -> Self {
        match value {
            1 => AggregationTemporality::Delta,
            2 => AggregationTemporality::Cumulative,
            _ => AggregationTemporality::Unspecified,
        }
    }
}

/// A single exemplar — a sampled measurement linking a metric point back to a
/// trace/span that contributed to it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Exemplar {
    #[schema(value_type = String, format = DateTime)]
    pub timestamp: DateTime<Utc>,
    /// The raw measured value of this exemplar.
    pub value: f64,
    /// The trace this exemplar links to, hex-encoded (absent when empty).
    pub trace_id: Option<String>,
    /// The span this exemplar links to, hex-encoded (absent when empty).
    pub span_id: Option<String>,
    /// Filtered attributes attached to this exemplar.
    pub attributes: BTreeMap<String, String>,
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
    /// Human-readable description from the OTel `Metric.description` field.
    #[serde(default)]
    pub description: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub timestamp: DateTime<Utc>,
    /// Start of the interval this point covers (from `start_time_unix_nano`).
    /// Used for delta/cumulative reasoning; absent when the producer omits it.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub start_time: Option<DateTime<Utc>>,
    /// Aggregation temporality (Sum/Histogram/ExponentialHistogram only).
    #[serde(default)]
    pub temporality: Option<AggregationTemporality>,
    /// Whether a Sum is monotonic (counter vs up-down counter). `None` for non-Sum.
    #[serde(default)]
    pub is_monotonic: Option<bool>,
    /// Data-point flags bitmask (e.g. NO_RECORDED_VALUE) from the OTLP point.
    #[serde(default)]
    pub flags: u32,
    /// For Gauge/Sum: the scalar value.
    pub value: Option<f64>,
    /// For Histogram/Summary/ExponentialHistogram: count of observations.
    pub histogram_count: Option<u64>,
    /// For Histogram/Summary/ExponentialHistogram: sum of observations.
    pub histogram_sum: Option<f64>,
    /// For Histogram: min value.
    pub histogram_min: Option<f64>,
    /// For Histogram: max value.
    pub histogram_max: Option<f64>,
    /// For Histogram: explicit bucket boundaries.
    pub histogram_bounds: Option<Vec<f64>>,
    /// For Histogram: count per bucket.
    pub histogram_bucket_counts: Option<Vec<u64>>,
    // ── ExponentialHistogram fields ──────────────────────────────────
    /// Resolution scale of the exponential buckets.
    #[serde(default)]
    pub exp_scale: Option<i32>,
    /// Count of values that are exactly zero (or within the zero threshold).
    #[serde(default)]
    pub exp_zero_count: Option<u64>,
    /// The threshold below which values are counted in `exp_zero_count`.
    #[serde(default)]
    pub exp_zero_threshold: Option<f64>,
    /// Bucket offset for the positive range.
    #[serde(default)]
    pub exp_positive_offset: Option<i32>,
    /// Per-bucket counts for the positive range.
    #[serde(default)]
    pub exp_positive_counts: Option<Vec<u64>>,
    /// Bucket offset for the negative range.
    #[serde(default)]
    pub exp_negative_offset: Option<i32>,
    /// Per-bucket counts for the negative range.
    #[serde(default)]
    pub exp_negative_counts: Option<Vec<u64>>,
    // ── Summary fields ───────────────────────────────────────────────
    /// Summary quantile/value pairs `(quantile, value)`.
    #[serde(default)]
    pub summary_quantiles: Option<Vec<(f64, f64)>>,
    /// Exemplars sampled for this data point, linking back to traces.
    #[serde(default)]
    pub exemplars: Vec<Exemplar>,
    /// Attribute labels on this data point.
    pub attributes: BTreeMap<String, String>,
}

impl MetricPoint {
    /// Construct a minimal Gauge-shaped `MetricPoint` with all optional
    /// full-fidelity fields defaulted. Helpers (decode, tests) fill in only the
    /// fields they care about; this keeps every call site compiling as the type
    /// grows.
    #[allow(clippy::too_many_arguments)]
    pub fn skeleton(
        project_id: i32,
        deployment_id: Option<i32>,
        resource: ResourceInfo,
        metric_name: String,
        metric_type: MetricType,
        unit: String,
        timestamp: DateTime<Utc>,
        attributes: BTreeMap<String, String>,
    ) -> Self {
        Self {
            project_id,
            deployment_id,
            resource,
            metric_name,
            metric_type,
            unit,
            description: None,
            timestamp,
            start_time: None,
            temporality: None,
            is_monotonic: None,
            flags: 0,
            value: None,
            histogram_count: None,
            histogram_sum: None,
            histogram_min: None,
            histogram_max: None,
            histogram_bounds: None,
            histogram_bucket_counts: None,
            exp_scale: None,
            exp_zero_count: None,
            exp_zero_threshold: None,
            exp_positive_offset: None,
            exp_positive_counts: None,
            exp_negative_offset: None,
            exp_negative_counts: None,
            summary_quantiles: None,
            exemplars: Vec::new(),
            attributes,
        }
    }
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
    /// Filter by span attributes (exact match on JSONB keys).
    /// e.g. {"gen_ai.system": "openai", "gen_ai.request.model": "gpt-4"}
    pub attributes: Option<BTreeMap<String, String>>,
    /// Filter by span name pattern (ILIKE).
    pub name_pattern: Option<String>,
    /// Field to sort the trace-summaries list by. Defaults to start time.
    #[serde(default)]
    pub sort_by: TraceSortField,
    /// Sort direction. Defaults to descending.
    #[serde(default)]
    pub sort_order: SortOrder,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Sortable fields for the trace-summaries list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceSortField {
    /// Trace start time (`MIN(start_time)`), the default.
    #[default]
    StartTime,
    /// Trace duration (`MAX(duration_ms)` — the longest span in the trace).
    Duration,
}

impl TraceSortField {
    /// Parse a query-string value into a sort field, defaulting to StartTime.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "duration" | "duration_ms" => TraceSortField::Duration,
            _ => TraceSortField::StartTime,
        }
    }
}

/// Sort direction for list queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Asc,
    #[default]
    Desc,
}

impl SortOrder {
    /// Parse a query-string value into a direction, defaulting to Desc.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "asc" | "ascending" => SortOrder::Asc,
            _ => SortOrder::Desc,
        }
    }

    /// The SQL keyword for this direction.
    pub fn as_sql(self) -> &'static str {
        match self {
            SortOrder::Asc => "ASC",
            SortOrder::Desc => "DESC",
        }
    }
}

/// Summary of a GenAI conversation — aggregated from OTel spans with `gen_ai.*` attributes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GenAiTraceSummary {
    pub trace_id: String,
    pub root_span_name: String,
    pub service_name: String,
    /// The GenAI provider (e.g. "openai", "anthropic") from `gen_ai.provider.name`.
    pub gen_ai_system: Option<String>,
    /// The requested model from `gen_ai.request.model`.
    pub gen_ai_model: Option<String>,
    /// The operation type from `gen_ai.operation.name` (e.g. "chat", "embeddings").
    pub gen_ai_operation: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    pub span_count: i64,
    pub error_count: i64,
    /// Total input tokens across all spans in this trace.
    pub total_input_tokens: Option<i64>,
    /// Total output tokens across all spans in this trace.
    pub total_output_tokens: Option<i64>,
    /// Total cache-creation input tokens across all spans.
    pub total_cache_creation_input_tokens: Option<i64>,
    /// Total cache-read input tokens across all spans.
    pub total_cache_read_input_tokens: Option<i64>,
}

/// A single GenAI span with extracted semantic convention fields.
///
/// Fields are aligned with the OpenTelemetry GenAI Semantic Conventions spec:
/// <https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-spans/>
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GenAiSpanDetail {
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: SpanKind,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    pub status_code: SpanStatusCode,

    // ── Core identification (Required/Conditionally Required) ────────
    /// The GenAI provider from `gen_ai.provider.name` (falls back to deprecated `gen_ai.system`).
    pub gen_ai_system: Option<String>,
    /// The operation type from `gen_ai.operation.name` (e.g. "chat", "embeddings", "execute_tool").
    pub gen_ai_operation: Option<String>,

    // ── Model information ────────────────────────────────────────────
    /// The requested model from `gen_ai.request.model`.
    pub gen_ai_model: Option<String>,
    /// The model that actually generated the response from `gen_ai.response.model`.
    pub gen_ai_response_model: Option<String>,

    // ── Request parameters (Recommended) ─────────────────────────────
    /// Temperature setting from `gen_ai.request.temperature`.
    pub request_temperature: Option<f64>,
    /// Max tokens from `gen_ai.request.max_tokens`.
    pub request_max_tokens: Option<i64>,
    /// Top-p setting from `gen_ai.request.top_p`.
    pub request_top_p: Option<f64>,
    /// Top-k setting from `gen_ai.request.top_k`.
    pub request_top_k: Option<f64>,
    /// Frequency penalty from `gen_ai.request.frequency_penalty`.
    pub request_frequency_penalty: Option<f64>,
    /// Presence penalty from `gen_ai.request.presence_penalty`.
    pub request_presence_penalty: Option<f64>,
    /// Stop sequences from `gen_ai.request.stop_sequences`.
    pub request_stop_sequences: Option<Vec<String>>,
    /// Seed for reproducibility from `gen_ai.request.seed`.
    pub request_seed: Option<i64>,
    /// Number of choices requested from `gen_ai.request.choice.count`.
    pub request_choice_count: Option<i64>,

    // ── Response information (Recommended) ───────────────────────────
    /// Unique completion ID from `gen_ai.response.id` (e.g. "chatcmpl-123").
    pub response_id: Option<String>,
    /// Reasons the model stopped from `gen_ai.response.finish_reasons` (e.g. ["stop"]).
    pub response_finish_reasons: Option<Vec<String>>,
    /// Output content type from `gen_ai.output.type` (text, json, image, speech).
    pub output_type: Option<String>,

    // ── Token usage (Recommended) ────────────────────────────────────
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    /// Tokens written to provider cache from `gen_ai.usage.cache_creation.input_tokens`.
    pub cache_creation_input_tokens: Option<i64>,
    /// Tokens served from provider cache from `gen_ai.usage.cache_read.input_tokens`.
    pub cache_read_input_tokens: Option<i64>,

    // ── Conversation tracking ────────────────────────────────────────
    /// Unique conversation/session/thread ID from `gen_ai.conversation.id`.
    pub conversation_id: Option<String>,

    // ── Error information ────────────────────────────────────────────
    /// Error type from `error.type` when the span status is ERROR.
    pub error_type: Option<String>,

    // ── Server information ───────────────────────────────────────────
    /// GenAI server address from `server.address`.
    pub server_address: Option<String>,
    /// GenAI server port from `server.port`.
    pub server_port: Option<i64>,

    // ── Agent attributes (Agent spans) ───────────────────────────────
    /// Agent identifier from `gen_ai.agent.id`.
    pub agent_id: Option<String>,
    /// Agent name from `gen_ai.agent.name`.
    pub agent_name: Option<String>,
    /// Agent description from `gen_ai.agent.description`.
    pub agent_description: Option<String>,
    /// Agent version from `gen_ai.agent.version`.
    pub agent_version: Option<String>,

    // ── Tool execution attributes (execute_tool spans) ───────────────
    /// Tool name from `gen_ai.tool.name`.
    pub tool_name: Option<String>,
    /// Tool call ID from `gen_ai.tool.call.id`.
    pub tool_call_id: Option<String>,
    /// Tool type from `gen_ai.tool.type` (function, extension, datastore).
    pub tool_type: Option<String>,
    /// Tool description from `gen_ai.tool.description`.
    pub tool_description: Option<String>,

    // ── Embeddings attributes ────────────────────────────────────────
    /// Output embedding dimensions from `gen_ai.embeddings.dimension.count`.
    pub embeddings_dimension_count: Option<i64>,
    /// Requested encoding formats from `gen_ai.request.encoding_formats`.
    pub request_encoding_formats: Option<Vec<String>>,

    // ── Retrieval attributes ─────────────────────────────────────────
    /// Data source identifier from `gen_ai.data_source.id`.
    pub data_source_id: Option<String>,

    // ── OpenAI-specific attributes ───────────────────────────────────
    /// OpenAI API type from `openai.api.type` (chat_completions, responses).
    pub openai_api_type: Option<String>,
    /// Requested service tier from `openai.request.service_tier`.
    pub openai_request_service_tier: Option<String>,
    /// Actual service tier from `openai.response.service_tier`.
    pub openai_response_service_tier: Option<String>,
    /// System fingerprint from `openai.response.system_fingerprint`.
    pub openai_system_fingerprint: Option<String>,

    // ── AWS Bedrock-specific attributes ──────────────────────────────
    /// AWS Bedrock guardrail ID from `aws.bedrock.guardrail.id`.
    pub aws_bedrock_guardrail_id: Option<String>,
    /// AWS Bedrock knowledge base ID from `aws.bedrock.knowledge_base.id`.
    pub aws_bedrock_knowledge_base_id: Option<String>,

    // ── Azure AI Inference-specific attributes ───────────────────────
    /// Azure resource provider namespace from `azure.resource_provider.namespace`.
    pub azure_resource_provider_namespace: Option<String>,

    // ── Opt-in content attributes ────────────────────────────────────
    /// Chat history input from `gen_ai.input.messages` (opt-in, JSON string).
    pub input_messages: Option<String>,
    /// Model output from `gen_ai.output.messages` (opt-in, JSON string).
    pub output_messages: Option<String>,
    /// System instructions from `gen_ai.system_instructions` (opt-in, JSON string).
    pub system_instructions: Option<String>,
    /// Tool definitions from `gen_ai.tool.definitions` (opt-in, JSON string).
    pub tool_definitions: Option<String>,
    /// Tool call arguments from `gen_ai.tool.call.arguments` (opt-in, JSON string).
    pub tool_call_arguments: Option<String>,
    /// Tool call result from `gen_ai.tool.call.result` (opt-in, JSON string).
    pub tool_call_result: Option<String>,
    /// Retrieval query text from `gen_ai.retrieval.query.text` (opt-in).
    pub retrieval_query_text: Option<String>,
    /// Retrieved documents from `gen_ai.retrieval.documents` (opt-in, JSON string).
    pub retrieval_documents: Option<String>,

    /// All span attributes for extensibility.
    pub attributes: BTreeMap<String, String>,
}

/// A GenAI-related event extracted from span events.
///
/// Covers `gen_ai.client.inference.operation.details` and `gen_ai.evaluation.result`
/// events per the OTel GenAI semantic conventions.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GenAiEvent {
    pub span_id: String,
    pub trace_id: String,
    pub event_name: String,
    #[schema(value_type = String, format = DateTime)]
    pub timestamp: DateTime<Utc>,
    /// All event attributes.
    pub attributes: BTreeMap<String, String>,
}

impl GenAiSpanDetail {
    /// Extract all GenAI semantic convention fields from a flat attributes map.
    ///
    /// This centralizes the mapping from OTel attribute keys to struct fields,
    /// handling both current and deprecated attribute names.
    #[allow(clippy::too_many_arguments)]
    pub fn from_span_attrs(
        span_id: String,
        parent_span_id: Option<String>,
        name: String,
        kind: SpanKind,
        start_time: DateTime<Utc>,
        duration_ms: f64,
        status_code: SpanStatusCode,
        attrs: BTreeMap<String, String>,
    ) -> Self {
        let get = |key: &str| -> Option<String> { attrs.get(key).cloned() };
        let get_f64 = |key: &str| -> Option<f64> { attrs.get(key).and_then(|v| v.parse().ok()) };
        let get_i64 = |key: &str| -> Option<i64> { attrs.get(key).and_then(|v| v.parse().ok()) };
        let get_or = |primary: &str, fallback: &str| -> Option<String> {
            attrs.get(primary).or_else(|| attrs.get(fallback)).cloned()
        };
        let get_i64_or = |primary: &str, fallback: &str| -> Option<i64> {
            attrs
                .get(primary)
                .or_else(|| attrs.get(fallback))
                .and_then(|v| v.parse().ok())
        };
        let get_string_array = |key: &str| -> Option<Vec<String>> {
            attrs.get(key).map(|v| {
                // Try JSON array first, then comma-separated
                serde_json::from_str::<Vec<String>>(v)
                    .unwrap_or_else(|_| v.split(',').map(|s| s.trim().to_string()).collect())
            })
        };

        Self {
            span_id,
            parent_span_id,
            name,
            kind,
            start_time,
            duration_ms,
            status_code,

            // Core identification (standard → deprecated → Vercel AI SDK fallback)
            gen_ai_system: get_or("gen_ai.provider.name", "gen_ai.system")
                .or_else(|| get("ai.model.provider")),
            gen_ai_operation: get_or("gen_ai.operation.name", "ai.operationId"),

            // Model (standard → Vercel AI SDK fallback)
            gen_ai_model: get_or("gen_ai.request.model", "ai.model.id"),
            gen_ai_response_model: get("gen_ai.response.model"),

            // Request parameters
            request_temperature: get_f64("gen_ai.request.temperature"),
            request_max_tokens: get_i64("gen_ai.request.max_tokens"),
            request_top_p: get_f64("gen_ai.request.top_p"),
            request_top_k: get_f64("gen_ai.request.top_k"),
            request_frequency_penalty: get_f64("gen_ai.request.frequency_penalty"),
            request_presence_penalty: get_f64("gen_ai.request.presence_penalty"),
            request_stop_sequences: get_string_array("gen_ai.request.stop_sequences"),
            request_seed: get_i64("gen_ai.request.seed"),
            request_choice_count: get_i64("gen_ai.request.choice.count"),

            // Response
            response_id: get("gen_ai.response.id"),
            response_finish_reasons: get_string_array("gen_ai.response.finish_reasons"),
            output_type: get("gen_ai.output.type"),

            // Token usage (standard → deprecated → Vercel AI SDK)
            input_tokens: get_i64_or("gen_ai.usage.input_tokens", "gen_ai.usage.prompt_tokens")
                .or_else(|| get_i64("ai.usage.promptTokens")),
            output_tokens: get_i64_or(
                "gen_ai.usage.output_tokens",
                "gen_ai.usage.completion_tokens",
            )
            .or_else(|| get_i64("ai.usage.completionTokens")),
            cache_creation_input_tokens: get_i64("gen_ai.usage.cache_creation.input_tokens"),
            cache_read_input_tokens: get_i64("gen_ai.usage.cache_read.input_tokens"),

            // Conversation
            conversation_id: get("gen_ai.conversation.id"),

            // Error
            error_type: get("error.type"),

            // Server
            server_address: get("server.address"),
            server_port: get_i64("server.port"),

            // Agent
            agent_id: get("gen_ai.agent.id"),
            agent_name: get("gen_ai.agent.name"),
            agent_description: get("gen_ai.agent.description"),
            agent_version: get("gen_ai.agent.version"),

            // Tool (standard → Vercel AI SDK fallback)
            tool_name: get_or("gen_ai.tool.name", "ai.toolCall.name"),
            tool_call_id: get_or("gen_ai.tool.call.id", "ai.toolCall.id"),
            tool_type: get("gen_ai.tool.type"),
            tool_description: get("gen_ai.tool.description"),

            // Embeddings
            embeddings_dimension_count: get_i64("gen_ai.embeddings.dimension.count"),
            request_encoding_formats: get_string_array("gen_ai.request.encoding_formats"),

            // Retrieval
            data_source_id: get("gen_ai.data_source.id"),

            // OpenAI-specific
            openai_api_type: get("openai.api.type"),
            openai_request_service_tier: get("openai.request.service_tier"),
            openai_response_service_tier: get("openai.response.service_tier"),
            openai_system_fingerprint: get("openai.response.system_fingerprint"),

            // AWS Bedrock-specific
            aws_bedrock_guardrail_id: get("aws.bedrock.guardrail.id"),
            aws_bedrock_knowledge_base_id: get("aws.bedrock.knowledge_base.id"),

            // Azure AI Inference-specific
            azure_resource_provider_namespace: get("azure.resource_provider.namespace"),

            // Opt-in content (standard → Vercel AI SDK fallback)
            input_messages: get_or("gen_ai.input.messages", "ai.prompt.messages"),
            output_messages: get("gen_ai.output.messages").or_else(|| {
                // Vercel AI SDK stores output as plain text in ai.response.text;
                // wrap it into the standard messages JSON array format.
                get("ai.response.text").map(|text| {
                    serde_json::json!([{"role": "assistant", "content": text}]).to_string()
                })
            }),
            system_instructions: get("gen_ai.system_instructions"),
            tool_definitions: get("gen_ai.tool.definitions"),
            tool_call_arguments: get_or("gen_ai.tool.call.arguments", "ai.toolCall.args"),
            tool_call_result: get_or("gen_ai.tool.call.result", "ai.toolCall.result"),
            retrieval_query_text: get("gen_ai.retrieval.query.text"),
            retrieval_documents: get("gen_ai.retrieval.documents"),

            attributes: attrs,
        }
    }

    /// Returns true if this span has any GenAI-related attributes.
    pub fn is_genai_span(&self) -> bool {
        self.gen_ai_system.is_some() || self.gen_ai_operation.is_some()
    }
}

/// Filter for querying metrics.
///
/// This is the **store-neutral query contract** every backend must satisfy.
/// The basic shape (`metric_name`/`service_name`/`environment`/time window /
/// `bucket_interval`/`limit`) is honoured by both ClickHouse and TimescaleDB.
/// The richer options (`metric_type`, `label_filters`, `group_by`,
/// `aggregation`) are populated by the query API and are forward-compatible:
/// each is `#[serde(default)]` so older callers and JSON payloads still parse.
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
    /// Restrict to a single metric type (gauge/sum/histogram/…). `None` = any.
    #[serde(default)]
    pub metric_type: Option<MetricType>,
    /// Exact-match data-point label filters as `(key, value)` pairs. ANDead
    /// together. Keys MUST pass the same allowlist as ingest before reaching a
    /// store; values are always bound, never concatenated.
    #[serde(default)]
    pub label_filters: Vec<(String, String)>,
    /// Label keys to group the series by, producing one bucket stream per
    /// distinct label-set. Keys MUST pass the allowlist. Empty = no grouping
    /// (one aggregate stream).
    #[serde(default)]
    pub group_by: Vec<String>,
    /// The aggregation reducing raw points into each bucket. Defaults to `Avg`.
    #[serde(default)]
    pub aggregation: MetricAggregation,
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

/// An explicit-bucket histogram aggregated over a time bucket.
///
/// Carries the reduced scalars (count/sum/min/max) plus the explicit bucket
/// layout — `bounds` (the upper bounds) and `bucket_counts` (observation counts,
/// summed element-wise across the window; length is `bounds.len() + 1`, the last
/// entry being the +Inf overflow bucket). With these, a caller can reconstruct
/// any quantile (e.g. p95) via cumulative-count interpolation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct HistogramSummary {
    /// Total observation count summed across the bucket window.
    pub count: u64,
    /// Sum of observed values across the bucket window.
    pub sum: f64,
    /// Minimum observed value, when reported by the producer.
    pub min: Option<f64>,
    /// Maximum observed value, when reported by the producer.
    pub max: Option<f64>,
    /// Explicit bucket upper bounds (OTLP `explicit_bounds`), ascending.
    pub bounds: Vec<f64>,
    /// Per-bucket observation counts summed element-wise across the window.
    /// Length is `bounds.len() + 1` (the trailing element is the +Inf bucket).
    pub bucket_counts: Vec<u64>,
}

/// A time-bucketed metric aggregate for chart display.
///
/// Store-neutral response contract. The legacy scalar fields
/// (`avg_value`/`min_value`/`max_value`/`count`) are always populated for chart
/// back-compat. The richer fields describe the explicitly-requested
/// [`MetricAggregation`] (`value`), optional `quantiles`, an optional
/// `histogram_summary`, and a `series_key` identifying the label-set when the
/// query used `group_by`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MetricBucket {
    #[schema(value_type = String, format = DateTime)]
    pub bucket: DateTime<Utc>,
    pub avg_value: f64,
    pub min_value: f64,
    pub max_value: f64,
    pub count: i64,
    /// The value of the requested [`MetricAggregation`] for this bucket. For the
    /// default `Avg` aggregation this equals `avg_value`. `#[serde(default)]` so
    /// pre-existing payloads (which only carried avg/min/max/count) still parse.
    #[serde(default)]
    pub value: f64,
    /// Computed quantile/value pairs `(quantile, value)` when the query asked for
    /// quantile aggregation; otherwise empty.
    #[serde(default)]
    pub quantiles: Vec<(f64, f64)>,
    /// A reduced histogram summary when the bucketed metric is a histogram.
    #[serde(default)]
    pub histogram_summary: Option<HistogramSummary>,
    /// The label-set this bucket belongs to, as ordered `(key, value)` pairs,
    /// when the query grouped by labels. Empty/`None` = the single ungrouped
    /// aggregate stream.
    #[serde(default)]
    pub series_key: Option<Vec<(String, String)>>,
}

impl MetricBucket {
    /// Construct a scalar bucket from the four legacy aggregate columns,
    /// defaulting the richer fields. `value` is set to `avg_value` so callers
    /// that don't request a specific aggregation get a sensible default.
    pub fn scalar(
        bucket: DateTime<Utc>,
        avg_value: f64,
        min_value: f64,
        max_value: f64,
        count: i64,
    ) -> Self {
        Self {
            bucket,
            avg_value,
            min_value,
            max_value,
            count,
            value: avg_value,
            quantiles: Vec::new(),
            histogram_summary: None,
            series_key: None,
        }
    }
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
        assert_eq!(
            MetricType::ExponentialHistogram.to_string(),
            "exponential_histogram"
        );
        assert_eq!(MetricType::Summary.to_string(), "summary");
    }

    #[test]
    fn test_aggregation_temporality_display() {
        assert_eq!(
            AggregationTemporality::Unspecified.to_string(),
            "unspecified"
        );
        assert_eq!(AggregationTemporality::Delta.to_string(), "delta");
        assert_eq!(AggregationTemporality::Cumulative.to_string(), "cumulative");
    }

    #[test]
    fn test_aggregation_temporality_from_proto() {
        assert_eq!(
            AggregationTemporality::from_proto(0),
            AggregationTemporality::Unspecified
        );
        assert_eq!(
            AggregationTemporality::from_proto(1),
            AggregationTemporality::Delta
        );
        assert_eq!(
            AggregationTemporality::from_proto(2),
            AggregationTemporality::Cumulative
        );
        // Unknown values fall back to Unspecified.
        assert_eq!(
            AggregationTemporality::from_proto(99),
            AggregationTemporality::Unspecified
        );
    }

    #[test]
    fn test_aggregation_temporality_serde_roundtrip() {
        let json = serde_json::to_string(&AggregationTemporality::Cumulative).unwrap();
        assert_eq!(json, "\"cumulative\"");
        let parsed: AggregationTemporality = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, AggregationTemporality::Cumulative);
    }

    // ── MetricAggregation ───────────────────────────────────────────

    #[test]
    fn test_metric_aggregation_default_is_avg() {
        assert_eq!(MetricAggregation::default(), MetricAggregation::Avg);
    }

    #[test]
    fn test_metric_aggregation_parse_keywords() {
        assert_eq!(MetricAggregation::parse("avg"), MetricAggregation::Avg);
        assert_eq!(MetricAggregation::parse("MEAN"), MetricAggregation::Avg);
        assert_eq!(MetricAggregation::parse("sum"), MetricAggregation::Sum);
        assert_eq!(MetricAggregation::parse("min"), MetricAggregation::Min);
        assert_eq!(MetricAggregation::parse("max"), MetricAggregation::Max);
        assert_eq!(MetricAggregation::parse("count"), MetricAggregation::Count);
        assert_eq!(
            MetricAggregation::parse("rate"),
            MetricAggregation::RatePerSec
        );
        // Unknown falls back to Avg.
        assert_eq!(MetricAggregation::parse("bogus"), MetricAggregation::Avg);
    }

    #[test]
    fn test_metric_aggregation_parse_quantiles() {
        assert_eq!(
            MetricAggregation::parse("p50"),
            MetricAggregation::Quantile(0.5)
        );
        assert_eq!(
            MetricAggregation::parse("p95"),
            MetricAggregation::Quantile(0.95)
        );
        assert_eq!(
            MetricAggregation::parse("p99"),
            MetricAggregation::Quantile(0.99)
        );
        assert_eq!(
            MetricAggregation::parse("p999"),
            MetricAggregation::Quantile(0.999)
        );
        assert_eq!(
            MetricAggregation::parse("quantile:0.95"),
            MetricAggregation::Quantile(0.95)
        );
        assert_eq!(
            MetricAggregation::parse("q0.9"),
            MetricAggregation::Quantile(0.9)
        );
        // Explicit out-of-range quantile falls back to Avg.
        assert_eq!(
            MetricAggregation::parse("quantile:2.0"),
            MetricAggregation::Avg
        );
        assert_eq!(MetricAggregation::parse("q1.5"), MetricAggregation::Avg);
    }

    #[test]
    fn test_metric_aggregation_quantile_accessor_clamps() {
        assert_eq!(MetricAggregation::Avg.quantile(), None);
        assert_eq!(MetricAggregation::Quantile(0.95).quantile(), Some(0.95));
        // Out-of-range stored value is clamped on read.
        assert_eq!(MetricAggregation::Quantile(1.5).quantile(), Some(1.0));
        assert_eq!(MetricAggregation::Quantile(-0.5).quantile(), Some(0.0));
    }

    #[test]
    fn test_metric_query_default_contract_fields() {
        let q = MetricQuery::default();
        assert!(q.metric_type.is_none());
        assert!(q.label_filters.is_empty());
        assert!(q.group_by.is_empty());
        assert_eq!(q.aggregation, MetricAggregation::Avg);
    }

    #[test]
    fn test_metric_bucket_scalar_defaults_rich_fields() {
        let now = chrono::Utc::now();
        let b = MetricBucket::scalar(now, 10.0, 5.0, 20.0, 3);
        assert_eq!(b.avg_value, 10.0);
        assert_eq!(b.min_value, 5.0);
        assert_eq!(b.max_value, 20.0);
        assert_eq!(b.count, 3);
        // `value` defaults to avg; richer fields are empty/None.
        assert_eq!(b.value, 10.0);
        assert!(b.quantiles.is_empty());
        assert!(b.histogram_summary.is_none());
        assert!(b.series_key.is_none());
    }

    #[test]
    fn test_metric_bucket_backcompat_deserialize_without_rich_fields() {
        // A payload from before the richer fields existed must still parse,
        // defaulting value/quantiles/histogram_summary/series_key.
        let json = r#"{"bucket":"2026-01-01T00:00:00Z","avg_value":1.0,"min_value":0.0,"max_value":2.0,"count":5}"#;
        let b: MetricBucket = serde_json::from_str(json).unwrap();
        assert_eq!(b.avg_value, 1.0);
        assert_eq!(b.value, 0.0); // default, no value key present
        assert!(b.quantiles.is_empty());
        assert!(b.series_key.is_none());
    }

    #[test]
    fn test_metric_point_skeleton_defaults() {
        let p = MetricPoint::skeleton(
            7,
            None,
            ResourceInfo::default(),
            "m".into(),
            MetricType::Gauge,
            "1".into(),
            chrono::Utc::now(),
            BTreeMap::new(),
        );
        assert_eq!(p.project_id, 7);
        assert!(p.temporality.is_none());
        assert!(p.is_monotonic.is_none());
        assert_eq!(p.flags, 0);
        assert!(p.exemplars.is_empty());
        assert!(p.summary_quantiles.is_none());
        assert!(p.exp_scale.is_none());
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

    // ── TraceQuery attribute filters ───────────────────────────────

    #[test]
    fn test_trace_query_default_has_no_attributes() {
        let q = TraceQuery::default();
        assert!(q.attributes.is_none());
        assert!(q.name_pattern.is_none());
    }

    #[test]
    fn test_trace_query_default_sort() {
        // Default list sort is newest-first by start time.
        let q = TraceQuery::default();
        assert_eq!(q.sort_by, TraceSortField::StartTime);
        assert_eq!(q.sort_order, SortOrder::Desc);
    }

    #[test]
    fn test_trace_sort_field_parse() {
        assert_eq!(TraceSortField::parse("duration"), TraceSortField::Duration);
        assert_eq!(
            TraceSortField::parse("duration_ms"),
            TraceSortField::Duration
        );
        assert_eq!(TraceSortField::parse("DURATION"), TraceSortField::Duration);
        assert_eq!(
            TraceSortField::parse("start_time"),
            TraceSortField::StartTime
        );
        // Unknown values fall back to the safe default.
        assert_eq!(TraceSortField::parse("bogus"), TraceSortField::StartTime);
    }

    #[test]
    fn test_sort_order_parse_and_sql() {
        assert_eq!(SortOrder::parse("asc"), SortOrder::Asc);
        assert_eq!(SortOrder::parse("ascending"), SortOrder::Asc);
        assert_eq!(SortOrder::parse("desc"), SortOrder::Desc);
        // Unknown values fall back to descending.
        assert_eq!(SortOrder::parse("sideways"), SortOrder::Desc);
        assert_eq!(SortOrder::Asc.as_sql(), "ASC");
        assert_eq!(SortOrder::Desc.as_sql(), "DESC");
    }

    #[test]
    fn test_trace_query_with_attributes() {
        let mut attrs = BTreeMap::new();
        attrs.insert("gen_ai.system".to_string(), "openai".to_string());
        attrs.insert("gen_ai.request.model".to_string(), "gpt-4".to_string());

        let q = TraceQuery {
            project_id: 1,
            attributes: Some(attrs.clone()),
            ..Default::default()
        };

        assert_eq!(q.attributes.as_ref().unwrap().len(), 2);
        assert_eq!(
            q.attributes.as_ref().unwrap().get("gen_ai.system").unwrap(),
            "openai"
        );
    }

    // ── GenAI types serde ──────────────────────────────────────────

    #[test]
    fn test_genai_trace_summary_serialization() {
        let summary = GenAiTraceSummary {
            trace_id: "abc123".into(),
            root_span_name: "chat".into(),
            service_name: "my-agent".into(),
            gen_ai_system: Some("openai".into()),
            gen_ai_model: Some("gpt-4".into()),
            gen_ai_operation: Some("chat".into()),
            start_time: chrono::Utc::now(),
            duration_ms: 1500.0,
            span_count: 3,
            error_count: 0,
            total_input_tokens: Some(100),
            total_output_tokens: Some(250),
            total_cache_creation_input_tokens: Some(50),
            total_cache_read_input_tokens: Some(30),
        };

        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"gen_ai_system\":\"openai\""));
        assert!(json.contains("\"total_input_tokens\":100"));
        assert!(json.contains("\"total_cache_creation_input_tokens\":50"));
    }

    #[test]
    fn test_genai_span_detail_serialization() {
        let mut attrs = BTreeMap::new();
        attrs.insert("gen_ai.system".to_string(), "anthropic".to_string());
        attrs.insert("gen_ai.usage.input_tokens".to_string(), "50".to_string());

        let detail = GenAiSpanDetail {
            span_id: "span1".into(),
            parent_span_id: None,
            name: "gen_ai.chat".into(),
            kind: SpanKind::Client,
            start_time: chrono::Utc::now(),
            duration_ms: 800.0,
            status_code: SpanStatusCode::Ok,
            gen_ai_system: Some("anthropic".into()),
            gen_ai_operation: Some("chat".into()),
            gen_ai_model: Some("claude-sonnet-4-20250514".into()),
            gen_ai_response_model: Some("claude-sonnet-4-20250514".into()),
            request_temperature: Some(0.7),
            request_max_tokens: Some(4096),
            request_top_p: None,
            request_top_k: None,
            request_frequency_penalty: None,
            request_presence_penalty: None,
            request_stop_sequences: None,
            request_seed: None,
            request_choice_count: None,
            response_id: Some("msg_abc123".into()),
            response_finish_reasons: Some(vec!["stop".into()]),
            output_type: Some("text".into()),
            input_tokens: Some(50),
            output_tokens: Some(200),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(30),
            conversation_id: Some("conv-1".into()),
            error_type: None,
            server_address: Some("api.anthropic.com".into()),
            server_port: Some(443),
            agent_id: None,
            agent_name: None,
            agent_description: None,
            agent_version: None,
            tool_name: None,
            tool_call_id: None,
            tool_type: None,
            tool_description: None,
            embeddings_dimension_count: None,
            request_encoding_formats: None,
            data_source_id: None,
            openai_api_type: None,
            openai_request_service_tier: None,
            openai_response_service_tier: None,
            openai_system_fingerprint: None,
            aws_bedrock_guardrail_id: None,
            aws_bedrock_knowledge_base_id: None,
            azure_resource_provider_namespace: None,
            input_messages: None,
            output_messages: None,
            system_instructions: None,
            tool_definitions: None,
            tool_call_arguments: None,
            tool_call_result: None,
            retrieval_query_text: None,
            retrieval_documents: None,
            attributes: attrs,
        };

        let json = serde_json::to_string(&detail).unwrap();
        let parsed: GenAiSpanDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.gen_ai_system.as_deref(), Some("anthropic"));
        assert_eq!(parsed.input_tokens, Some(50));
        assert_eq!(parsed.output_tokens, Some(200));
        assert_eq!(parsed.response_id.as_deref(), Some("msg_abc123"));
        assert_eq!(parsed.conversation_id.as_deref(), Some("conv-1"));
        assert_eq!(parsed.cache_read_input_tokens, Some(30));
        assert_eq!(parsed.request_temperature, Some(0.7));
    }

    // ── from_span_attrs extraction tests ─────────────────────────────

    fn make_detail(attrs: Vec<(&str, &str)>) -> GenAiSpanDetail {
        let map: BTreeMap<String, String> = attrs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        GenAiSpanDetail::from_span_attrs(
            "span-1".into(),
            None,
            "test-span".into(),
            SpanKind::Client,
            chrono::Utc::now(),
            100.0,
            SpanStatusCode::Ok,
            map,
        )
    }

    #[test]
    fn test_from_span_attrs_openai_chat_completion() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.operation.name", "chat"),
            ("gen_ai.request.model", "gpt-4o"),
            ("gen_ai.response.model", "gpt-4o-2024-08-06"),
            ("gen_ai.request.temperature", "0.9"),
            ("gen_ai.request.max_tokens", "2048"),
            ("gen_ai.request.top_p", "0.95"),
            ("gen_ai.request.frequency_penalty", "0.5"),
            ("gen_ai.request.presence_penalty", "0.3"),
            ("gen_ai.request.seed", "42"),
            ("gen_ai.request.choice.count", "2"),
            ("gen_ai.request.stop_sequences", r#"["END","STOP"]"#),
            ("gen_ai.response.id", "chatcmpl-abc123"),
            ("gen_ai.response.finish_reasons", r#"["stop"]"#),
            ("gen_ai.output.type", "text"),
            ("gen_ai.usage.input_tokens", "150"),
            ("gen_ai.usage.output_tokens", "300"),
            ("gen_ai.conversation.id", "thread-xyz"),
            ("server.address", "api.openai.com"),
            ("server.port", "443"),
            ("openai.api.type", "chat_completions"),
            ("openai.request.service_tier", "auto"),
            ("openai.response.service_tier", "default"),
            ("openai.response.system_fingerprint", "fp_abc123"),
        ]);

        assert_eq!(d.gen_ai_system.as_deref(), Some("openai"));
        assert_eq!(d.gen_ai_operation.as_deref(), Some("chat"));
        assert_eq!(d.gen_ai_model.as_deref(), Some("gpt-4o"));
        assert_eq!(
            d.gen_ai_response_model.as_deref(),
            Some("gpt-4o-2024-08-06")
        );
        assert_eq!(d.request_temperature, Some(0.9));
        assert_eq!(d.request_max_tokens, Some(2048));
        assert_eq!(d.request_top_p, Some(0.95));
        assert_eq!(d.request_frequency_penalty, Some(0.5));
        assert_eq!(d.request_presence_penalty, Some(0.3));
        assert_eq!(d.request_seed, Some(42));
        assert_eq!(d.request_choice_count, Some(2));
        assert_eq!(
            d.request_stop_sequences,
            Some(vec!["END".to_string(), "STOP".to_string()])
        );
        assert_eq!(d.response_id.as_deref(), Some("chatcmpl-abc123"));
        assert_eq!(d.response_finish_reasons, Some(vec!["stop".to_string()]));
        assert_eq!(d.output_type.as_deref(), Some("text"));
        assert_eq!(d.input_tokens, Some(150));
        assert_eq!(d.output_tokens, Some(300));
        assert_eq!(d.conversation_id.as_deref(), Some("thread-xyz"));
        assert_eq!(d.server_address.as_deref(), Some("api.openai.com"));
        assert_eq!(d.server_port, Some(443));
        assert_eq!(d.openai_api_type.as_deref(), Some("chat_completions"));
        assert_eq!(d.openai_request_service_tier.as_deref(), Some("auto"));
        assert_eq!(d.openai_response_service_tier.as_deref(), Some("default"));
        assert_eq!(d.openai_system_fingerprint.as_deref(), Some("fp_abc123"));
        assert!(d.is_genai_span());
    }

    #[test]
    fn test_from_span_attrs_anthropic_with_cache_tokens() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "anthropic"),
            ("gen_ai.operation.name", "chat"),
            ("gen_ai.request.model", "claude-sonnet-4-20250514"),
            ("gen_ai.response.model", "claude-sonnet-4-20250514"),
            ("gen_ai.usage.input_tokens", "100"),
            ("gen_ai.usage.output_tokens", "250"),
            ("gen_ai.usage.cache_creation.input_tokens", "80"),
            ("gen_ai.usage.cache_read.input_tokens", "60"),
            ("gen_ai.response.id", "msg_01abc"),
            ("gen_ai.response.finish_reasons", r#"["end_turn"]"#),
        ]);

        assert_eq!(d.gen_ai_system.as_deref(), Some("anthropic"));
        assert_eq!(d.input_tokens, Some(100));
        assert_eq!(d.output_tokens, Some(250));
        assert_eq!(d.cache_creation_input_tokens, Some(80));
        assert_eq!(d.cache_read_input_tokens, Some(60));
        assert_eq!(
            d.response_finish_reasons,
            Some(vec!["end_turn".to_string()])
        );
    }

    #[test]
    fn test_from_span_attrs_deprecated_gen_ai_system_fallback() {
        // Old instrumentation uses gen_ai.system instead of gen_ai.provider.name
        let d = make_detail(vec![
            ("gen_ai.system", "openai"),
            ("gen_ai.operation.name", "chat"),
            ("gen_ai.request.model", "gpt-4"),
        ]);

        assert_eq!(d.gen_ai_system.as_deref(), Some("openai"));
        assert_eq!(d.gen_ai_model.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn test_from_span_attrs_provider_name_overrides_deprecated_system() {
        // When both are present, gen_ai.provider.name should win
        let d = make_detail(vec![
            ("gen_ai.provider.name", "anthropic"),
            ("gen_ai.system", "old-value"),
            ("gen_ai.operation.name", "chat"),
        ]);

        assert_eq!(d.gen_ai_system.as_deref(), Some("anthropic"));
    }

    #[test]
    fn test_from_span_attrs_deprecated_token_field_fallback() {
        // Old instrumentation uses prompt_tokens/completion_tokens
        let d = make_detail(vec![
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.usage.prompt_tokens", "50"),
            ("gen_ai.usage.completion_tokens", "100"),
        ]);

        assert_eq!(d.input_tokens, Some(50));
        assert_eq!(d.output_tokens, Some(100));
    }

    #[test]
    fn test_from_span_attrs_new_token_fields_override_deprecated() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.usage.input_tokens", "50"),
            ("gen_ai.usage.prompt_tokens", "999"),
            ("gen_ai.usage.output_tokens", "100"),
            ("gen_ai.usage.completion_tokens", "999"),
        ]);

        assert_eq!(d.input_tokens, Some(50));
        assert_eq!(d.output_tokens, Some(100));
    }

    #[test]
    fn test_from_span_attrs_embeddings_operation() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.operation.name", "embeddings"),
            ("gen_ai.request.model", "text-embedding-3-small"),
            ("gen_ai.embeddings.dimension.count", "1536"),
            ("gen_ai.request.encoding_formats", r#"["float","base64"]"#),
            ("gen_ai.usage.input_tokens", "8"),
        ]);

        assert_eq!(d.gen_ai_operation.as_deref(), Some("embeddings"));
        assert_eq!(d.embeddings_dimension_count, Some(1536));
        assert_eq!(
            d.request_encoding_formats,
            Some(vec!["float".to_string(), "base64".to_string()])
        );
        assert_eq!(d.input_tokens, Some(8));
        assert!(d.output_tokens.is_none());
    }

    #[test]
    fn test_from_span_attrs_retrieval_operation() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "pinecone"),
            ("gen_ai.operation.name", "retrieval"),
            ("gen_ai.data_source.id", "my-knowledge-base"),
            ("gen_ai.request.model", "text-embedding-3-small"),
            ("gen_ai.request.top_k", "5"),
            ("gen_ai.retrieval.query.text", "What is GenAI?"),
            ("gen_ai.retrieval.documents", r#"[{"id":"doc1"}]"#),
        ]);

        assert_eq!(d.gen_ai_operation.as_deref(), Some("retrieval"));
        assert_eq!(d.data_source_id.as_deref(), Some("my-knowledge-base"));
        assert_eq!(d.request_top_k, Some(5.0));
        assert_eq!(d.retrieval_query_text.as_deref(), Some("What is GenAI?"));
        assert!(d.retrieval_documents.is_some());
    }

    #[test]
    fn test_from_span_attrs_execute_tool_operation() {
        let d = make_detail(vec![
            ("gen_ai.operation.name", "execute_tool"),
            ("gen_ai.tool.name", "get_weather"),
            ("gen_ai.tool.call.id", "call_abc123"),
            ("gen_ai.tool.type", "function"),
            ("gen_ai.tool.description", "Get current weather"),
            ("gen_ai.tool.call.arguments", r#"{"city":"London"}"#),
            ("gen_ai.tool.call.result", r#"{"temp":22}"#),
        ]);

        assert_eq!(d.gen_ai_operation.as_deref(), Some("execute_tool"));
        assert_eq!(d.tool_name.as_deref(), Some("get_weather"));
        assert_eq!(d.tool_call_id.as_deref(), Some("call_abc123"));
        assert_eq!(d.tool_type.as_deref(), Some("function"));
        assert_eq!(d.tool_description.as_deref(), Some("Get current weather"));
        assert_eq!(
            d.tool_call_arguments.as_deref(),
            Some(r#"{"city":"London"}"#)
        );
        assert_eq!(d.tool_call_result.as_deref(), Some(r#"{"temp":22}"#));
    }

    #[test]
    fn test_from_span_attrs_agent_invoke_span() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.operation.name", "invoke_agent"),
            ("gen_ai.agent.id", "agent-001"),
            ("gen_ai.agent.name", "Research Assistant"),
            ("gen_ai.agent.description", "Helps with research tasks"),
            ("gen_ai.agent.version", "2.0.0"),
            ("gen_ai.request.model", "gpt-4o"),
            ("gen_ai.conversation.id", "conv-123"),
        ]);

        assert_eq!(d.gen_ai_operation.as_deref(), Some("invoke_agent"));
        assert_eq!(d.agent_id.as_deref(), Some("agent-001"));
        assert_eq!(d.agent_name.as_deref(), Some("Research Assistant"));
        assert_eq!(
            d.agent_description.as_deref(),
            Some("Helps with research tasks")
        );
        assert_eq!(d.agent_version.as_deref(), Some("2.0.0"));
        assert_eq!(d.conversation_id.as_deref(), Some("conv-123"));
    }

    #[test]
    fn test_from_span_attrs_aws_bedrock_provider() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "aws.bedrock"),
            ("gen_ai.operation.name", "chat"),
            ("gen_ai.request.model", "anthropic.claude-3-sonnet"),
            ("aws.bedrock.guardrail.id", "sgi5gkybzqak"),
            ("aws.bedrock.knowledge_base.id", "XFWUPB9PAW"),
        ]);

        assert_eq!(d.gen_ai_system.as_deref(), Some("aws.bedrock"));
        assert_eq!(d.aws_bedrock_guardrail_id.as_deref(), Some("sgi5gkybzqak"));
        assert_eq!(
            d.aws_bedrock_knowledge_base_id.as_deref(),
            Some("XFWUPB9PAW")
        );
    }

    #[test]
    fn test_from_span_attrs_azure_ai_inference_provider() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "azure.ai.inference"),
            ("gen_ai.operation.name", "chat"),
            ("gen_ai.request.model", "gpt-4o"),
            (
                "azure.resource_provider.namespace",
                "Microsoft.CognitiveServices",
            ),
            ("server.address", "my-endpoint.openai.azure.com"),
            ("server.port", "443"),
        ]);

        assert_eq!(d.gen_ai_system.as_deref(), Some("azure.ai.inference"));
        assert_eq!(
            d.azure_resource_provider_namespace.as_deref(),
            Some("Microsoft.CognitiveServices")
        );
        assert_eq!(
            d.server_address.as_deref(),
            Some("my-endpoint.openai.azure.com")
        );
    }

    #[test]
    fn test_from_span_attrs_error_span() {
        let map: BTreeMap<String, String> = [
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.operation.name", "chat"),
            ("error.type", "RateLimitError"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let d = GenAiSpanDetail::from_span_attrs(
            "span-err".into(),
            None,
            "chat gpt-4".into(),
            SpanKind::Client,
            chrono::Utc::now(),
            50.0,
            SpanStatusCode::Error,
            map,
        );

        assert_eq!(d.status_code, SpanStatusCode::Error);
        assert_eq!(d.error_type.as_deref(), Some("RateLimitError"));
    }

    #[test]
    fn test_from_span_attrs_opt_in_content_fields() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.operation.name", "chat"),
            (
                "gen_ai.input.messages",
                r#"[{"role":"user","content":"Hi"}]"#,
            ),
            (
                "gen_ai.output.messages",
                r#"[{"role":"assistant","content":"Hello!"}]"#,
            ),
            (
                "gen_ai.system_instructions",
                r#"[{"content":"Be helpful"}]"#,
            ),
            ("gen_ai.tool.definitions", r#"[{"name":"get_weather"}]"#),
        ]);

        assert!(d.input_messages.is_some());
        assert!(d.output_messages.is_some());
        assert!(d.system_instructions.is_some());
        assert!(d.tool_definitions.is_some());
    }

    #[test]
    fn test_from_span_attrs_non_genai_span() {
        // A plain HTTP span that's part of a GenAI trace
        let d = make_detail(vec![
            ("http.method", "POST"),
            ("http.url", "https://api.openai.com/v1/chat/completions"),
            ("http.status_code", "200"),
        ]);

        assert!(d.gen_ai_system.is_none());
        assert!(d.gen_ai_operation.is_none());
        assert!(!d.is_genai_span());
        // The raw attributes are still preserved
        assert_eq!(d.attributes.get("http.method").unwrap(), "POST");
    }

    #[test]
    fn test_from_span_attrs_empty_attributes() {
        let d = make_detail(vec![]);

        assert!(d.gen_ai_system.is_none());
        assert!(d.gen_ai_operation.is_none());
        assert!(d.gen_ai_model.is_none());
        assert!(d.input_tokens.is_none());
        assert!(d.output_tokens.is_none());
        assert!(!d.is_genai_span());
    }

    #[test]
    fn test_from_span_attrs_comma_separated_string_array() {
        // Some instrumentations may send arrays as comma-separated strings
        let d = make_detail(vec![
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.response.finish_reasons", "stop, length"),
            ("gen_ai.request.stop_sequences", "END, DONE"),
        ]);

        assert_eq!(
            d.response_finish_reasons,
            Some(vec!["stop".to_string(), "length".to_string()])
        );
        assert_eq!(
            d.request_stop_sequences,
            Some(vec!["END".to_string(), "DONE".to_string()])
        );
    }

    #[test]
    fn test_from_span_attrs_invalid_numeric_values_ignored() {
        let d = make_detail(vec![
            ("gen_ai.provider.name", "openai"),
            ("gen_ai.usage.input_tokens", "not-a-number"),
            ("gen_ai.request.temperature", "invalid"),
            ("server.port", "abc"),
        ]);

        assert!(d.input_tokens.is_none());
        assert!(d.request_temperature.is_none());
        assert!(d.server_port.is_none());
    }

    #[test]
    fn test_genai_event_serialization() {
        let event = GenAiEvent {
            span_id: "span-1".into(),
            trace_id: "trace-1".into(),
            event_name: "gen_ai.client.inference.operation.details".into(),
            timestamp: chrono::Utc::now(),
            attributes: BTreeMap::from([
                ("gen_ai.usage.input_tokens".to_string(), "100".to_string()),
                ("gen_ai.response.id".to_string(), "chatcmpl-abc".to_string()),
            ]),
        };

        let json = serde_json::to_string(&event).unwrap();
        let parsed: GenAiEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.event_name,
            "gen_ai.client.inference.operation.details"
        );
        assert_eq!(
            parsed.attributes.get("gen_ai.usage.input_tokens").unwrap(),
            "100"
        );
    }
}
