//! Pluggable storage backend for OTel data.
//!
//! The [`OtelStorage`] trait defines the contract for storing and querying
//! metrics, traces, and logs. The default implementation uses TimescaleDB,
//! but alternative backends (ClickHouse, etc.) can be plugged in by
//! implementing this trait.

pub mod clickhouse;
pub mod timescaledb;

use async_trait::async_trait;

use crate::error::OtelError;
use crate::types::{
    GenAiEvent, GenAiSpanDetail, GenAiTraceSummary, HealthSummary, Insight, InsightStatus,
    LogQuery, LogRecord, MetricBucket, MetricPoint, MetricQuery, SpanRecord, StorageQuota,
    TraceQuery, TraceSummary,
};

/// Result type for storage operations.
pub type StorageResult<T> = Result<T, OtelError>;

/// The pluggable storage backend trait for OTel data.
///
/// All storage operations are async and return `StorageResult`.
/// Implementations must be `Send + Sync` for use across async tasks.
///
/// # Implementing a new backend
///
/// ```rust,ignore
/// struct ClickHouseStorage { /* ... */ }
///
/// #[async_trait]
/// impl OtelStorage for ClickHouseStorage {
///     async fn store_metrics(&self, points: Vec<MetricPoint>) -> StorageResult<u64> {
///         // ... ClickHouse-specific batch insert
///     }
///     // ... remaining methods
/// }
/// ```
#[async_trait]
pub trait OtelStorage: Send + Sync {
    // ── Write operations ────────────────────────────────────────────

    /// Batch-insert metric data points.
    /// Returns the number of points successfully stored.
    async fn store_metrics(&self, points: Vec<MetricPoint>) -> StorageResult<u64>;

    /// Batch-insert trace spans.
    /// Returns the number of spans successfully stored.
    async fn store_spans(&self, spans: Vec<SpanRecord>) -> StorageResult<u64>;

    /// Batch-insert log records into the fast-query store (DB).
    /// Typically only ERROR/WARN records are routed here.
    /// Returns the number of records successfully stored.
    async fn store_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64>;

    /// Store log records into the cold archive (S3 as NDJSON).
    /// All severity levels are archived.
    /// Returns the number of records archived.
    async fn archive_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64>;

    // ── Read operations ─────────────────────────────────────────────

    /// Query metric time series, returning bucketed aggregates.
    async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>>;

    /// List distinct metric names for a project.
    async fn list_metric_names(&self, project_id: i32) -> StorageResult<Vec<String>>;

    /// Query trace spans matching the given filters.
    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>>;

    /// Query trace summaries for the list view — one row per trace with
    /// span count, error count, and root span info. Pagination applies
    /// to traces (not individual spans).
    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>>;

    /// Count distinct traces matching the given filters (for pagination).
    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64>;

    /// Get all spans for a single trace ID.
    async fn get_trace(&self, project_id: i32, trace_id: &str) -> StorageResult<Vec<SpanRecord>>;

    /// Query log records from the fast-query store.
    async fn query_logs(&self, query: LogQuery) -> StorageResult<Vec<LogRecord>>;

    // ── GenAI queries ────────────────────────────────────────────────

    /// Query GenAI trace summaries — traces that contain spans with `gen_ai.*` attributes.
    /// Returns aggregated per-trace summaries with model, system, and token counts.
    async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<GenAiTraceSummary>>;

    /// Get all spans for a GenAI trace — includes both GenAI-attributed spans
    /// and their child spans (HTTP, DB, tool execution, etc.) to show the full
    /// trace tree. Spans are enriched with extracted semantic convention fields.
    async fn get_genai_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiSpanDetail>>;

    /// Count distinct GenAI traces matching the given filters.
    async fn count_genai_traces(&self, query: TraceQuery) -> StorageResult<u64>;

    /// Get GenAI-related events from span events in a trace.
    /// Returns events matching `gen_ai.*` event names.
    async fn get_genai_trace_events(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiEvent>>;

    // ── Insights ────────────────────────────────────────────────────

    /// Store or update an insight (anomaly correlation).
    async fn upsert_insight(&self, insight: &Insight) -> StorageResult<i64>;

    /// List insights for a project, optionally filtered by status.
    async fn list_insights(
        &self,
        project_id: i32,
        status: Option<InsightStatus>,
        limit: u64,
        offset: u64,
    ) -> StorageResult<Vec<Insight>>;

    /// Resolve an active insight.
    async fn resolve_insight(&self, insight_id: i64) -> StorageResult<()>;

    // ── Health summaries ────────────────────────────────────────────

    /// Write a pre-computed health summary.
    async fn store_health_summary(&self, summary: &HealthSummary) -> StorageResult<()>;

    /// Get the latest health summaries for a project.
    async fn get_health_summaries(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> StorageResult<Vec<HealthSummary>>;

    // ── Quota management ────────────────────────────────────────────

    /// Get current storage usage for a project.
    async fn get_storage_quota(&self, project_id: i32) -> StorageResult<StorageQuota>;

    /// Check if a project has exceeded its storage quota.
    async fn check_quota(&self, project_id: i32) -> StorageResult<bool>;

    // ── Anomaly detection helpers ───────────────────────────────────

    /// Get the average and stddev for a metric over a lookback window,
    /// grouped by hour-of-day and day-of-week for time-aware baselines.
    async fn get_metric_baseline(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        lookback_days: i32,
    ) -> StorageResult<Vec<BaselinePoint>>;

    /// Get recent 1-minute aggregates for anomaly scoring.
    async fn get_recent_minute_aggregates(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        minutes: i32,
    ) -> StorageResult<Vec<MinuteAggregate>>;

    /// Get recent deploy events for correlation.
    async fn get_recent_deploys(
        &self,
        project_id: i32,
        minutes: i32,
    ) -> StorageResult<Vec<DeployEvent>>;

    // ── Data management ─────────────────────────────────────────────

    /// Apply retention policies (delete data older than configured limits).
    /// Returns the number of rows affected.
    async fn apply_retention(&self, project_id: i32) -> StorageResult<u64>;

    /// Get P95 latency for a service over a time window (for sampling decisions).
    async fn get_p95_latency(
        &self,
        project_id: i32,
        service_name: &str,
        window_minutes: i32,
    ) -> StorageResult<f64>;
}

/// A baseline data point for anomaly detection.
#[derive(Debug, Clone)]
pub struct BaselinePoint {
    pub hour_of_day: i32,
    pub day_of_week: i32,
    pub avg_value: f64,
    pub stddev_value: f64,
    pub sample_count: i64,
}

/// A 1-minute aggregate for recent data.
#[derive(Debug, Clone)]
pub struct MinuteAggregate {
    pub bucket: chrono::DateTime<chrono::Utc>,
    pub avg_value: f64,
    pub count: i64,
}

/// A deploy event for anomaly correlation.
#[derive(Debug, Clone)]
pub struct DeployEvent {
    pub deployment_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployed_at: chrono::DateTime<chrono::Utc>,
    pub service_name: Option<String>,
}
