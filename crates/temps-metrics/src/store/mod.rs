pub mod clickhouse;
pub mod clickhouse_migrations;
pub mod timescale;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

use crate::error::MetricsError;

/// Returns `true` when a metric should be treated as a cumulative monotonic
/// counter for query purposes — i.e. the raw values must be LAG-differenced
/// to produce a meaningful rate-of-change chart.
///
/// OTLP cumulative Sum metrics (RustFS, etc.) are stored as raw Gauge values
/// in `service_metrics` to avoid double-delta corruption. This flag tells the
/// query layer to apply the LAG window function at read time.
pub fn is_monotonic_counter(metric_name: &str) -> bool {
    // OpenMetrics/Prometheus convention: _total suffix = cumulative counter.
    // Also match common patterns from OTLP exporters.
    metric_name.ends_with("_total")
        || metric_name.ends_with(".total")
        || metric_name.ends_with("_count")
        || metric_name.ends_with(".count")
}

/// Convert a UI `range` string (`1h`, `6h`, `24h`, `7d`) to
/// `(window_duration, bucket_step)` for a [`RangeQuery`]. Unknown ranges
/// fall back to the 1-hour window.
pub fn range_to_step(range: &str) -> (Duration, Duration) {
    match range {
        "1h" => (Duration::hours(1), Duration::minutes(1)),
        "6h" => (Duration::hours(6), Duration::minutes(5)),
        "24h" => (Duration::hours(24), Duration::minutes(15)),
        "7d" => (Duration::days(7), Duration::hours(1)),
        _ => (Duration::hours(1), Duration::minutes(1)),
    }
}

/// The kind of metric value being stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricKind {
    /// A point-in-time measurement (e.g. memory usage, active connections).
    Gauge,
    /// A monotonically increasing counter; the store records deltas.
    Counter,
}

/// The category of entity that emitted the metric, used to resolve `source_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// `source_id` refers to `external_services.id`
    Database,
    /// `source_id` refers to `deployments.id`
    Deployment,
    /// `source_id` refers to `deployment_containers.id`
    Container,
    /// `source_id` refers to `nodes.id`
    Node,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Database => "database",
            SourceKind::Deployment => "deployment",
            SourceKind::Container => "container",
            SourceKind::Node => "node",
        }
    }
}

/// A single metric observation ready to be persisted.
#[derive(Debug, Clone)]
pub struct MetricPoint {
    /// Timestamp of the observation (always UTC).
    pub time: DateTime<Utc>,
    /// Category of entity that emitted the metric.
    pub source_kind: SourceKind,
    /// Primary-key ID of the entity in the corresponding table.
    pub source_id: i32,
    /// Dotted metric name, e.g. `"pg.connections_active"`.
    pub name: String,
    /// Numeric value.
    ///
    /// # Safety contract for `MetricKind::Counter`
    ///
    /// For counter metrics, `value` **must** be a non-negative delta
    /// (`current_reading − previous_reading`), not the raw cumulative counter.
    /// The store does **not** perform delta computation — that is the
    /// responsibility of the scraper/collector.
    ///
    /// On the first scrape after a server restart, the in-memory baseline is
    /// lost. The collector should skip writing a delta for that cycle rather
    /// than writing the raw cumulative value as a delta (which would produce a
    /// spurious spike on graphs).
    ///
    /// TODO(metrics): Issue 8 — persist last_values to a
    /// `metric_counter_checkpoints` table so the baseline survives restarts and
    /// process restarts on monitored services can be distinguished from
    /// server restarts.
    pub value: f64,
    /// Whether this is a gauge or a counter delta.
    pub kind: MetricKind,
    /// Database/service engine name (e.g. `"postgres"`, `"redis"`).
    pub engine: Option<String>,
    /// Deployment environment name (e.g. `"production"`).
    pub environment: Option<String>,
    /// Node ID (for node-scoped metrics).
    pub node_id: Option<i32>,
    /// Arbitrary key-value labels for filtering (indexed with GIN).
    pub labels: HashMap<String, String>,
}

/// Parameters for a time-range query that returns bucketed series data.
#[derive(Debug, Clone)]
pub struct RangeQuery {
    pub source_kind: SourceKind,
    pub source_id: i32,
    pub name: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Requested bucket width. The store may coerce this to the nearest
    /// supported interval (e.g. 1 h when falling back to hourly aggregate).
    pub step: Duration,
    /// When `true` the metric is a cumulative monotonic counter stored as raw
    /// values (OTLP path). The store computes per-bucket deltas using a LAG
    /// window function instead of returning raw averages. Resets (where the
    /// current value < previous value) are treated as 0 for that bucket.
    pub monotonic: bool,
}

/// Parameters for a latest-value query across a set of metric names.
#[derive(Debug, Clone)]
pub struct LatestQuery {
    pub source_kind: SourceKind,
    pub source_id: i32,
    /// Metric names to retrieve; returns only those that have at least one row.
    pub names: Vec<String>,
}

/// Parameters for a per-label-value latest query.
///
/// Returns the most-recent value of each requested metric **grouped by the
/// distinct values of a single label key** (e.g. `datname` for Postgres
/// per-database metrics). Used to build a breakdown table — one row per
/// database — instead of collapsing every `datname` series into one number.
#[derive(Debug, Clone)]
pub struct LatestByLabelQuery {
    pub source_kind: SourceKind,
    pub source_id: i32,
    /// Metric names to retrieve (e.g. `pg.database_size_bytes`,
    /// `pg.cache_hit_ratio`).
    pub names: Vec<String>,
    /// Label key to group by (e.g. `"datname"`). Only rows that carry this
    /// label key are returned; the instance-wide aggregate row (which lacks it)
    /// is excluded.
    pub label_key: String,
}

/// One `(label_value, metric_name, value)` triple from a [`LatestByLabelQuery`].
#[derive(Debug, Clone)]
pub struct LabelledMetric {
    /// The value of the grouped label key (e.g. the database name).
    pub label_value: String,
    pub name: String,
    pub value: f64,
}

/// Abstraction over the metrics storage backend.
#[async_trait]
pub trait MetricsStore: Send + Sync {
    /// Persist a batch of metric points. Uses bulk insert for efficiency.
    async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError>;

    /// Return bucketed `(timestamp, avg_value)` pairs for the requested range.
    ///
    /// Table selection (TimescaleDB implementation; ClickHouse may differ):
    /// - range ≤ 7 days  → raw `service_metrics` (full resolution, avoids the
    ///   hourly CA's 1-hour trailing staleness window)
    /// - range ≤ 90 days → `service_metrics_hourly` continuous aggregate
    /// - range > 90 days → `service_metrics_daily`  continuous aggregate
    ///
    /// **Trailing-edge gap:** continuous aggregates have a refresh lag equal to
    /// their `end_offset` (1 hour for hourly, 1 day for daily).  Data in that
    /// window exists in the raw table but may not yet appear in the aggregate.
    ///
    /// **7-day boundary race:** raw retention is also 7 days.  A query range
    /// that straddles the exact 7-day boundary may encounter a gap of up to
    /// 2 hours where data has been dropped from the raw table but has not yet
    /// been refreshed into the hourly aggregate.  This is expected behaviour
    /// and is not surfaced as an error.
    ///
    /// # FIXME(metrics-scale): Issue 2 — overlap the raw+hourly tables with a
    /// UNION query at the 7-day boundary to eliminate the leading-edge data gap,
    /// or extend the raw retention to `7 days + hourly start_offset (2 h)`.
    async fn query_range(
        &self,
        filter: RangeQuery,
    ) -> Result<Vec<(DateTime<Utc>, f64)>, MetricsError>;

    /// Return the most-recent value for each requested metric name.
    async fn query_latest(&self, filter: LatestQuery)
        -> Result<HashMap<String, f64>, MetricsError>;

    /// Return the most-recent value of each requested metric, grouped by the
    /// distinct values of a single label key (e.g. `datname`). One entry per
    /// `(label_value, metric_name)`. Rows lacking the label key (the
    /// instance-wide aggregate) are excluded.
    async fn query_latest_by_label(
        &self,
        filter: LatestByLabelQuery,
    ) -> Result<Vec<LabelledMetric>, MetricsError>;

    /// Return the timestamp of the most-recent metric row for a source, or
    /// `None` if no metrics have ever been recorded. Used by the UI to show
    /// "last received at …" so users can confirm the pipeline is alive.
    async fn latest_timestamp(
        &self,
        source_kind: SourceKind,
        source_id: i32,
    ) -> Result<Option<DateTime<Utc>>, MetricsError>;

    /// Delete raw metric rows older than `older_than`.
    /// Continuous-aggregate retention is managed by TimescaleDB refresh
    /// policies; this method only touches the raw `service_metrics` table.
    async fn prune(&self, older_than: DateTime<Utc>) -> Result<u64, MetricsError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_range_to_step_1h() {
        let (window, step) = range_to_step("1h");
        assert_eq!(window, Duration::hours(1));
        assert_eq!(step, Duration::minutes(1));
    }

    #[test]
    fn test_range_to_step_7d() {
        let (window, step) = range_to_step("7d");
        assert_eq!(window, Duration::days(7));
        assert_eq!(step, Duration::hours(1));
    }

    #[test]
    fn test_range_to_step_unknown_defaults_to_1h() {
        let (window, step) = range_to_step("30d");
        assert_eq!(window, Duration::hours(1));
        assert_eq!(step, Duration::minutes(1));
    }

    #[test]
    fn test_is_monotonic_counter_suffixes() {
        assert!(is_monotonic_counter("pg.blks_read_total"));
        assert!(is_monotonic_counter("mongo.op_insert.total"));
        assert!(is_monotonic_counter("http.requests_count"));
        assert!(!is_monotonic_counter("container.cpu_percent"));
        assert!(!is_monotonic_counter("container.memory_used_bytes"));
        assert!(!is_monotonic_counter("container.network_rx_bytes_delta"));
    }
}
