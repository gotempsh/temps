// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod buffered;
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

/// Metrics that are stored as per-interval **deltas** (one non-negative count
/// per sampler cycle), as opposed to point-in-time gauges.
///
/// These must be `SUM`med per bucket at read time. `AVG` would report the mean
/// count per sampling cycle, which neither grows with the bucket width nor adds
/// up to a window total: a 5-minute bucket over 60s samples would show ~1/5 of
/// the real traffic, and summing the buckets for a "total requests" card would
/// undercount by `bucket / sample_interval`.
///
/// They are not caught by [`is_monotonic_counter`] on purpose — that flag means
/// "raw cumulative value, LAG-difference me" and these are already deltas.
/// TimescaleDB does not persist [`MetricKind`]. ClickHouse does, but
/// [`MetricKind::Counter`] alone cannot distinguish cumulative counters from
/// interval deltas, so both read paths identify these metrics by name.
/// The proxy sampler's `is_counter` samples are checked against this list by
/// a test in `temps-proxy` so the two cannot drift apart.
pub const DELTA_COUNTER_METRICS: &[&str] = &[
    "proxy.requests",
    "proxy.requests_1xx",
    "proxy.requests_2xx",
    "proxy.requests_3xx",
    "proxy.requests_4xx",
    "proxy.requests_5xx",
    "proxy.requests_project",
    "proxy.requests_console",
    "proxy.requests_other",
    "proxy.streaming_sessions",
    "proxy.rate_limited",
    "proxy.ip_access_denied",
    "proxy.log_write_retries",
    "proxy.log_batches_dropped",
    "proxy.log_entries_dropped",
    "proxy.ingress_shadow_evaluations",
    "proxy.ingress_shadow_mismatches",
];

/// Returns `true` when `metric_name` is a per-interval delta counter that
/// range queries must `SUM` per bucket. See [`DELTA_COUNTER_METRICS`].
pub fn is_delta_counter(metric_name: &str) -> bool {
    DELTA_COUNTER_METRICS.contains(&metric_name)
}

/// SQL aggregate used to collapse the raw samples of one bucket for a
/// non-cumulative metric: `SUM` for delta counters, `AVG` for gauges.
///
/// Returns a `&'static str` chosen from a closed set, so it is safe to
/// interpolate into SQL.
pub(crate) fn bucket_aggregate(metric_name: &str) -> &'static str {
    if is_delta_counter(metric_name) {
        "SUM"
    } else {
        "AVG"
    }
}

/// Widen `requested` so `window / step` stays ≤ [`temps_core::time_window::MAX_SERIES_POINTS`].
///
/// A 7-day window at 1 minute is 10_080 points. Preset steps from
/// [`duration_to_step`] already sit under the cap; this is the backstop when a
/// caller passes a finer `RangeQuery::step` than the window can support.
pub fn clamp_step(window: Duration, requested: Duration) -> Duration {
    let window_secs = window.num_seconds().max(1);
    let requested_secs = requested.num_seconds().max(1);
    let min_step_secs = temps_core::time_window::min_step_secs(window_secs);
    Duration::seconds(requested_secs.max(min_step_secs))
}

/// Bucket step for a metric range query, matching the node-metrics UI
/// presets: 1m for ≤1h, 5m for ≤6h, 15m for ≤24h, 1h otherwise.
/// Always passed through [`clamp_step`] so a wide window cannot emit 1-minute
/// buckets.
pub fn duration_to_step(window: Duration) -> Duration {
    let requested = if window <= Duration::hours(1) {
        Duration::minutes(1)
    } else if window <= Duration::hours(6) {
        Duration::minutes(5)
    } else if window <= Duration::hours(24) {
        Duration::minutes(15)
    } else {
        Duration::hours(1)
    };
    clamp_step(window, requested)
}

/// Convert a UI `range` string (`1h`, `6h`, `24h`, `7d`) to
/// `(window_duration, bucket_step)` for a [`RangeQuery`]. Unknown ranges
/// fall back to the 1-hour window.
pub fn range_to_step(range: &str) -> (Duration, Duration) {
    let window = match range {
        "1h" => Duration::hours(1),
        "6h" => Duration::hours(6),
        "24h" => Duration::hours(24),
        "7d" => Duration::days(7),
        _ => Duration::hours(1),
    };
    (window, duration_to_step(window))
}

/// What a range-series bucket without any samples reads as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapFill {
    /// "No data": the chart breaks the line instead of drawing a value
    /// nobody measured. Right for gauges (0% CPU while the sampler was down
    /// would be a lie) and for cumulative counters, whose increase over a gap
    /// is unknown until the next sample.
    Null,
    /// The bucket is a real zero. Right for per-interval delta counters
    /// ([`is_delta_counter`]): the proxy that served requests is the process
    /// that samples them, so no sample means no request went through it.
    Zero,
}

/// [`GapFill`] policy for one range query.
pub fn gap_fill_for(metric_name: &str, monotonic: bool) -> GapFill {
    if !monotonic && is_delta_counter(metric_name) {
        GapFill::Zero
    } else {
        GapFill::Null
    }
}

/// Bucket width both stores use for `filter`, when it is safe to gap-fill.
///
/// Up to 7 days both backends bucket the raw table at
/// [`clamp_step`]`(window, step)`, aligned to the Unix epoch for every width
/// that divides a day (TimescaleDB's default `time_bucket` origin is a UTC
/// midnight; ClickHouse's `toStartOfInterval` is epoch-based). Wider windows
/// read continuous aggregates whose width the two backends choose
/// differently, so they are returned as stored rather than filled on a grid
/// that might not match.
pub fn gap_fill_step(filter: &RangeQuery) -> Option<Duration> {
    let window = filter.to - filter.from;
    if window <= Duration::zero() || window > Duration::days(7) {
        return None;
    }
    let step = clamp_step(window, filter.step);
    let secs = step.num_seconds();
    (secs > 0 && 86_400 % secs == 0).then_some(step)
}

/// Return one point per `step` bucket of `[from, to]`: the stored value where
/// the store produced one, `fill` where it did not.
///
/// The stores only emit buckets that hold samples, so a stretch where the
/// sampler (or the whole server) was down simply vanished from the series and
/// charts drew a straight line across it. Points the store returns off the
/// expected grid are kept, never dropped.
pub fn fill_range_gaps(
    points: Vec<(DateTime<Utc>, f64)>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    step: Duration,
    fill: GapFill,
) -> Vec<(DateTime<Utc>, Option<f64>)> {
    let step_ms = step.num_milliseconds().max(1);
    let filler = match fill {
        GapFill::Null => None,
        GapFill::Zero => Some(0.0),
    };
    let mut by_time: std::collections::BTreeMap<i64, (DateTime<Utc>, Option<f64>)> = points
        .into_iter()
        .map(|(time, value)| (time.timestamp_millis(), (time, Some(value))))
        .collect();
    let last_ms = to.timestamp_millis();
    let mut slot = from.timestamp_millis().div_euclid(step_ms) * step_ms;
    while slot <= last_ms {
        if let Some(time) = DateTime::<Utc>::from_timestamp_millis(slot) {
            by_time.entry(slot).or_insert((time, filler));
        }
        slot += step_ms;
    }
    by_time.into_values().collect()
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

    /// Return bucketed `(timestamp, value)` pairs for the requested range.
    ///
    /// The per-bucket value is `AVG` for gauges, `SUM` for per-interval delta
    /// counters ([`is_delta_counter`]), and the LAG-differenced increase for
    /// cumulative counters (`RangeQuery::monotonic`).
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

    /// [`Self::query_range`] with every bucket of the window present, so a
    /// chart can tell "no samples" from a value. Empty buckets are `None` or
    /// `Some(0.0)` per [`gap_fill_for`]. Windows [`gap_fill_step`] cannot fill
    /// safely (over 7 days) come back as stored, every value `Some`.
    async fn query_range_filled(
        &self,
        filter: RangeQuery,
    ) -> Result<Vec<(DateTime<Utc>, Option<f64>)>, MetricsError> {
        let fill = gap_fill_for(&filter.name, filter.monotonic);
        let step = gap_fill_step(&filter);
        let (from, to) = (filter.from, filter.to);
        let points = self.query_range(filter).await?;
        Ok(match step {
            Some(step) => fill_range_gaps(points, from, to, step, fill),
            None => points.into_iter().map(|(t, v)| (t, Some(v))).collect(),
        })
    }

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

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(secs, 0).unwrap()
    }

    fn range(from: i64, to: i64, step_secs: i64) -> RangeQuery {
        RangeQuery {
            source_kind: SourceKind::Node,
            source_id: 0,
            name: "node.cpu_percent".into(),
            from: at(from),
            to: at(to),
            step: Duration::seconds(step_secs),
            monotonic: false,
        }
    }

    #[test]
    fn fill_range_gaps_breaks_a_gauge_across_a_three_hour_gap() {
        // 24h of 15-minute buckets, the agent silent for hours 10..13.
        let from = 1_760_000_400; // aligned to 900s
        let gap = (from + 10 * 3_600)..(from + 13 * 3_600);
        let points: Vec<_> = (0..96)
            .map(|i| from + i * 900)
            .filter(|t| !gap.contains(t))
            .map(|t| (at(t), 42.0))
            .collect();
        assert_eq!(points.len(), 96 - 12);

        let filled = fill_range_gaps(
            points,
            at(from),
            at(from + 86_400 - 1),
            Duration::seconds(900),
            GapFill::Null,
        );
        assert_eq!(filled.len(), 96);
        for (time, value) in &filled {
            if gap.contains(&time.timestamp()) {
                assert_eq!(
                    *value, None,
                    "gap bucket {time} must be null, not interpolated"
                );
            } else {
                assert_eq!(*value, Some(42.0));
            }
        }
    }

    #[test]
    fn fill_range_gaps_zero_fills_delta_counters_and_aligns_unaligned_windows() {
        // 10:07 -> 10:41 at 15m covers the 10:00, 10:15 and 10:30 buckets.
        let hour = 1_760_000_400;
        let filled = fill_range_gaps(
            vec![(at(hour + 900), 7.0)],
            at(hour + 7 * 60),
            at(hour + 41 * 60),
            Duration::seconds(900),
            GapFill::Zero,
        );
        assert_eq!(
            filled,
            vec![
                (at(hour), Some(0.0)),
                (at(hour + 900), Some(7.0)),
                (at(hour + 1_800), Some(0.0)),
            ]
        );
    }

    #[test]
    fn fill_range_gaps_keeps_off_grid_points() {
        let filled = fill_range_gaps(
            vec![(at(1_000_030), 1.0)],
            at(1_000_020),
            at(1_000_080),
            Duration::seconds(60),
            GapFill::Null,
        );
        assert!(filled.contains(&(at(1_000_030), Some(1.0))));
    }

    #[test]
    fn gap_fill_policy_is_zero_only_for_delta_counters() {
        assert_eq!(gap_fill_for("proxy.requests", false), GapFill::Zero);
        assert_eq!(gap_fill_for("node.cpu_percent", false), GapFill::Null);
        assert_eq!(
            gap_fill_for("proxy.request_duration_p95_ms", false),
            GapFill::Null
        );
        assert_eq!(
            gap_fill_for("node.network_rx_bytes_total", true),
            GapFill::Null
        );
    }

    #[test]
    fn gap_fill_step_only_fills_raw_table_windows() {
        assert_eq!(
            gap_fill_step(&range(0, 86_400, 900)),
            Some(Duration::seconds(900))
        );
        assert_eq!(
            gap_fill_step(&range(0, 7 * 86_400, 3_600)),
            Some(Duration::seconds(3_600))
        );
        // Continuous aggregates: the two backends disagree on the width.
        assert_eq!(gap_fill_step(&range(0, 30 * 86_400, 3_600)), None);
        assert_eq!(gap_fill_step(&range(10, 10, 60)), None);
    }

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
    fn test_duration_to_step_matches_preset_windows() {
        assert_eq!(duration_to_step(Duration::hours(1)), Duration::minutes(1));
        assert_eq!(duration_to_step(Duration::hours(6)), Duration::minutes(5));
        assert_eq!(duration_to_step(Duration::hours(24)), Duration::minutes(15));
        assert_eq!(duration_to_step(Duration::days(7)), Duration::hours(1));
    }

    #[test]
    fn test_duration_to_step_custom_windows() {
        assert_eq!(
            duration_to_step(Duration::minutes(90)),
            Duration::minutes(5)
        );
        assert_eq!(duration_to_step(Duration::hours(12)), Duration::minutes(15));
        assert_eq!(duration_to_step(Duration::hours(48)), Duration::hours(1));
    }

    #[test]
    fn test_clamp_step_coarsens_1m_over_7d() {
        let window = Duration::days(7);
        let clamped = clamp_step(window, Duration::minutes(1));
        let points =
            temps_core::time_window::bucket_count(window.num_seconds(), clamped.num_seconds());
        assert!(
            points <= temps_core::time_window::MAX_SERIES_POINTS,
            "7d at 1m must not emit {points} points"
        );
        assert!(clamped > Duration::minutes(1));
    }

    #[test]
    fn test_clamp_step_preserves_preset_7d_hourly() {
        let window = Duration::days(7);
        assert_eq!(clamp_step(window, Duration::hours(1)), Duration::hours(1));
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

    #[test]
    fn test_is_delta_counter_matches_proxy_request_counters_only() {
        assert!(is_delta_counter("proxy.requests"));
        assert!(is_delta_counter("proxy.requests_5xx"));
        assert!(is_delta_counter("proxy.requests_console"));
        assert!(is_delta_counter("proxy.streaming_sessions"));
        // Gauges derived from the same interval must keep averaging.
        assert!(!is_delta_counter("proxy.error_rate_percent"));
        assert!(!is_delta_counter("proxy.request_duration_p95_ms"));
        assert!(!is_delta_counter("proxy.streaming_duration_avg_ms"));
        assert!(!is_delta_counter("node.cpu_percent"));
        // Prefix matches are not enough: names are matched exactly.
        assert!(!is_delta_counter("proxy.requests_other_thing"));
    }

    #[test]
    fn test_delta_counters_are_not_treated_as_monotonic() {
        // Delta counters are pre-differenced; LAG-differencing them again would
        // corrupt the series.
        for name in DELTA_COUNTER_METRICS {
            assert!(
                !is_monotonic_counter(name),
                "{name} is a delta counter and must not be LAG-differenced"
            );
        }
    }

    #[test]
    fn test_bucket_aggregate_selection() {
        assert_eq!(bucket_aggregate("proxy.requests"), "SUM");
        assert_eq!(bucket_aggregate("proxy.request_duration_p95_ms"), "AVG");
        assert_eq!(bucket_aggregate("pg.connections_active"), "AVG");
    }
}
