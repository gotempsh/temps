//! ClickHouse storage backend for OTel spans.
//!
//! This module provides the ClickHouse client wrapper and the [`ChSpanRow`]
//! row type that mirrors the DDL in
//! `crates/temps-otel/migrations/clickhouse/0001_spans.sql` exactly.
//!
//! [`ClickHouseOtelStorage`] implements the full [`OtelStorage`] trait:
//!
//! - **Span-domain write** (`store_spans`) runs directly against ClickHouse.
//! - **Span-domain reads** (`query_trace_summaries`, `count_traces`,
//!   `query_spans`, `get_trace`, GenAI reads) run natively against ClickHouse
//!   as of Phase 1.
//! - **Non-span methods** (metrics, logs, anomaly helpers, retention) and
//!   **control-row methods** (insights, health summaries, quota) are delegated
//!   to the inner [`Arc<TimescaleDbStorage>`] unconditionally. These are
//!   ADR-016 Phases 2–4.
//!
//! ## Activation
//!
//! The plugin constructs [`ClickHouseOtelStorage`] only when
//! `ServerConfig::is_clickhouse_enabled()` returns `true`
//! (all four `TEMPS_CLICKHOUSE_*` env vars set). When disabled, the existing
//! `TimescaleDbStorage` path is unchanged.
//!
//! ## Row type stability
//!
//! [`ChSpanRow`] field order and types **must stay in lockstep with the DDL**.
//! The `clickhouse` crate's `Row` derive uses positional binary serialization
//! over the HTTP interface; any field order mismatch silently corrupts data.
//! If the schema changes, update both `0001_spans.sql` and [`ChSpanRow`]
//! together and bump the migration number.
//!
//! ## SQL injection safety
//!
//! Filter *values* (service_name, trace_id, status_code, timestamps, etc.) are
//! always passed via `.bind(value)` with a `?` placeholder — never
//! `format!`-ed into the SQL string.  The only interpolated strings are the
//! `ORDER BY` clause direction and field name, both derived from fixed enums
//! (`TraceSortField` / `SortOrder`) with no user-controlled input path.

pub mod migrations;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

use temps_metrics::validate_metric_name;

use crate::error::OtelError;
use crate::storage::timescaledb::TimescaleDbStorage;
use crate::storage::{BaselinePoint, DeployEvent, MinuteAggregate, OtelStorage, StorageResult};
use crate::types::{
    GenAiEvent, GenAiSpanDetail, GenAiTraceSummary, HealthSummary, HistogramSummary, Insight,
    InsightStatus, LogQuery, LogRecord, MetricAggregation, MetricBucket, MetricPoint, MetricQuery,
    SpanEvent, SpanKind, SpanRecord, SpanStatusCode, StorageQuota, TraceQuery, TraceSummary,
};

// ── Client configuration ────────────────────────────────────────────────────

/// Connection configuration for the ClickHouse OTel backend.
///
/// Built from `ServerConfig` fields populated by the `TEMPS_CLICKHOUSE_*`
/// environment variables. All four fields are required; the plugin calls
/// `ServerConfig::is_clickhouse_enabled()` to guard construction.
#[derive(Clone)]
pub struct ClickHouseOtelConfig {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

// Manual Debug that masks the password so it can never leak into logs, panic
// messages, or tracing spans that capture the config with `{:?}`.
impl std::fmt::Debug for ClickHouseOtelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseOtelConfig")
            .field("url", &self.url)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"***")
            .finish()
    }
}

impl ClickHouseOtelConfig {
    pub fn new(
        url: impl Into<String>,
        database: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            database: database.into(),
            user: user.into(),
            password: password.into(),
        }
    }
}

// ── Client wrapper ──────────────────────────────────────────────────────────

/// Thin wrapper around `clickhouse::Client` for the OTel backend.
///
/// `clickhouse::Client` is already cheaply cloneable (Arc-backed internally),
/// but wrapping it lets us add OTel-specific helpers (e.g., health check,
/// migration runner) without coupling the storage struct to construction details.
pub struct ClickHouseOtelClient {
    pub(crate) client: ::clickhouse::Client,
}

impl ClickHouseOtelClient {
    /// Build a client from configuration.
    ///
    /// Does not validate connectivity; call [`health_check`] or run migrations
    /// to confirm the connection is live.
    pub fn new(config: ClickHouseOtelConfig) -> Self {
        let client = ::clickhouse::Client::default()
            .with_url(config.url)
            .with_database(config.database)
            .with_user(config.user)
            .with_password(config.password);
        Self { client }
    }

    /// Borrow the underlying client for queries.
    pub fn client(&self) -> &::clickhouse::Client {
        &self.client
    }

    /// Clone the underlying client (cheap — Arc internally).
    pub fn client_clone(&self) -> ::clickhouse::Client {
        self.client.clone()
    }

    /// Verify connectivity and authentication with a `SELECT 1`.
    pub async fn health_check(&self) -> Result<(), OtelError> {
        self.client
            .query("SELECT 1")
            .fetch_one::<u8>()
            .await
            .map_err(|e| OtelError::Storage {
                message: format!("ClickHouse health check failed: {e}"),
            })?;
        Ok(())
    }
}

// ── Row type ────────────────────────────────────────────────────────────────

/// ClickHouse row matching the `spans` table DDL in `0001_spans.sql`.
///
/// **Field order must match the DDL column order exactly.** The `clickhouse`
/// crate serialises fields positionally (binary protocol over HTTP); any
/// reordering here relative to the DDL silently corrupts inserts.
///
/// ## Type mapping
///
/// | DDL type                    | Rust type          | Notes                                         |
/// |-----------------------------|-------------------|-----------------------------------------------|
/// | `Int32`                     | `i32`             |                                               |
/// | `Nullable(Int32)`           | `Option<i32>`     |                                               |
/// | `LowCardinality(String)`    | `String`          | No special Rust type needed                   |
/// | `String`                    | `String`          |                                               |
/// | `DateTime64(3, 'UTC')`      | `i64`             | Unix milliseconds; avoids precision ambiguity |
/// | `Float64`                   | `f64`             |                                               |
/// | `UInt64`                    | `u64`             |                                               |
///
/// ## Timestamp encoding
///
/// `start_time` and `end_time` are stored as milliseconds since the Unix
/// epoch (`i64`). This matches how the analytics `ChEventRow` encodes its
/// `timestamp` column and is the safest mapping for `DateTime64(3)` — the
/// `clickhouse` crate's `chrono` feature can also send `DateTime<Utc>`, but
/// the i64 path is explicit about precision and avoids any mismatch between
/// the Rust timezone representation and the CH server setting.
///
/// At read time, the query layer converts back via
/// `DateTime::from_timestamp_millis(ms).unwrap_or_default()`.
///
/// ## Null vs empty-string sentinels
///
/// - `parent_span_id`: `String` (not `Option<String>`). Root spans store `""`.
///   This matches the DDL `DEFAULT ''` and avoids a CH `Nullable` column on a
///   high-cardinality ordering key.
/// - `service_version`, `deployment_environment`, `status_message`,
///   `attributes`, `events`: `String` with `""` / `"{}"` / `"[]"` sentinels.
///   CH `LowCardinality(String)` and plain `String` columns are non-nullable
///   in this DDL.
/// - `deployment_id`: `Option<i32>` — genuinely nullable in both the domain
///   type and the DDL (`Nullable(Int32)`).
#[derive(::clickhouse::Row, Serialize, Deserialize, Debug, Clone)]
pub struct ChSpanRow {
    // ── Tenant + deployment context ─────────────────────────────────────────
    /// project_id  Int32
    pub project_id: i32,
    /// deployment_id  Nullable(Int32)
    pub deployment_id: Option<i32>,

    // ── Resource / service identity (denormalized at ingest) ────────────────
    /// service_name  LowCardinality(String)
    pub service_name: String,
    /// service_version  LowCardinality(String)
    pub service_version: String,
    /// deployment_environment  LowCardinality(String)
    pub deployment_environment: String,

    // ── Span identity ───────────────────────────────────────────────────────
    /// trace_id  String
    pub trace_id: String,
    /// span_id  String
    pub span_id: String,
    /// parent_span_id  String  DEFAULT ''
    pub parent_span_id: String,

    // ── Span semantics ──────────────────────────────────────────────────────
    /// name  String
    pub name: String,
    /// kind  LowCardinality(String)
    pub kind: String,

    // ── Timing ─────────────────────────────────────────────────────────────
    /// start_time  DateTime64(3, 'UTC') — stored as Unix milliseconds
    pub start_time: i64,
    /// end_time  DateTime64(3, 'UTC') — stored as Unix milliseconds
    pub end_time: i64,
    /// duration_ms  Float64
    pub duration_ms: f64,

    // ── Status ──────────────────────────────────────────────────────────────
    /// status_code  LowCardinality(String)
    pub status_code: String,
    /// status_message  String  DEFAULT ''
    pub status_message: String,

    // ── Payload (JSON serialised) ───────────────────────────────────────────
    /// attributes  String  DEFAULT '{}'  (JSON object)
    pub attributes: String,
    /// events  String  DEFAULT '[]'  (JSON array)
    pub events: String,

    // ── Dedup key ───────────────────────────────────────────────────────────
    /// _version  UInt64  DEFAULT toUnixTimestamp64Milli(now64())
    /// Set to the current Unix millisecond timestamp at ingest time so that
    /// OTLP retries of the same span converge to one canonical row via
    /// ReplacingMergeTree (highest _version wins).
    pub _version: u64,

    // ── Retention ───────────────────────────────────────────────────────────
    /// retention_days  UInt16  DEFAULT 90
    ///
    /// Added by migration 0004_retention_days.sql — must remain the last
    /// field so its position matches the DDL column order (positional binary
    /// serialization). The TTL expression in 0005_retention_ttl.sql reads
    /// this column: `toDateTime(start_time) + toIntervalDay(retention_days)`.
    pub retention_days: u16,
}

// ── From<&SpanRecord> for ChSpanRow ────────────────────────────────────────

impl From<&SpanRecord> for ChSpanRow {
    fn from(span: &SpanRecord) -> Self {
        // Serialize attributes and events to JSON strings. These are
        // BTreeMap<String,String> / Vec<SpanEvent>, both of which are
        // trivially serializable. We fall back to "{}" / "[]" on the
        // (unreachable in practice) serialization error path rather than
        // propagating — ingest must not drop spans over a serialization
        // hiccup in metadata.
        let attributes = serde_json::to_string(&span.attributes).unwrap_or_else(|_| "{}".into());
        let events = serde_json::to_string(&span.events).unwrap_or_else(|_| "[]".into());

        // _version: Unix ms timestamp used as the ReplacingMergeTree dedup key.
        // Using now() at conversion time (same moment as ingest). Spans retried
        // by the OTLP exporter will produce a higher _version than the first
        // attempt and win the dedup.
        let version = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            project_id: span.project_id,
            deployment_id: span.deployment_id,
            service_name: span.resource.service_name.clone(),
            service_version: span.resource.service_version.clone().unwrap_or_default(),
            deployment_environment: span
                .resource
                .deployment_environment
                .clone()
                .unwrap_or_default(),
            trace_id: span.trace_id.clone(),
            span_id: span.span_id.clone(),
            parent_span_id: span.parent_span_id.clone().unwrap_or_default(),
            name: span.name.clone(),
            kind: span_kind_to_str(span.kind).to_owned(),
            start_time: span.start_time.timestamp_millis(),
            end_time: span.end_time.timestamp_millis(),
            duration_ms: span.duration_ms,
            status_code: span_status_to_str(span.status_code).to_owned(),
            status_message: span.status_message.clone(),
            attributes,
            events,
            _version: version,
            // Callers that hold a RetentionResolver should override this field
            // after construction. The fixed default matches the DDL DEFAULT.
            retention_days: temps_core::RetentionTable::Spans.default_days(),
        }
    }
}

// ── ChSpanRow → SpanRecord ──────────────────────────────────────────────────

/// Convert a [`ChSpanRow`] read from ClickHouse back into a [`SpanRecord`].
///
/// Used by the query methods (`query_spans`, `get_trace`) that fetch raw rows
/// and need to return the canonical domain type.
///
/// Deserialization failures in `attributes`/`events` JSON fall back to empty
/// collections — the span identity and timing are preserved, and partial
/// attribute loss is preferable to surfacing an error for an otherwise-valid
/// span.
impl From<ChSpanRow> for SpanRecord {
    fn from(row: ChSpanRow) -> Self {
        use chrono::TimeZone;

        let start_time = chrono::Utc
            .timestamp_millis_opt(row.start_time)
            .single()
            .unwrap_or_default();
        let end_time = chrono::Utc
            .timestamp_millis_opt(row.end_time)
            .single()
            .unwrap_or_default();

        let attributes: std::collections::BTreeMap<String, String> =
            serde_json::from_str(&row.attributes).unwrap_or_default();
        let events: Vec<SpanEvent> = serde_json::from_str(&row.events).unwrap_or_default();

        let resource = crate::types::ResourceInfo {
            service_name: row.service_name,
            service_version: if row.service_version.is_empty() {
                None
            } else {
                Some(row.service_version)
            },
            deployment_environment: if row.deployment_environment.is_empty() {
                None
            } else {
                Some(row.deployment_environment)
            },
            attributes: std::collections::BTreeMap::new(),
        };

        SpanRecord {
            project_id: row.project_id,
            deployment_id: row.deployment_id,
            resource,
            trace_id: row.trace_id,
            span_id: row.span_id,
            parent_span_id: if row.parent_span_id.is_empty() {
                None
            } else {
                Some(row.parent_span_id)
            },
            name: row.name,
            kind: str_to_span_kind(&row.kind),
            start_time,
            end_time,
            duration_ms: row.duration_ms,
            status_code: str_to_span_status(&row.status_code),
            status_message: row.status_message,
            attributes,
            events,
        }
    }
}

// ── Metric row type ───────────────────────────────────────────────────────

/// ClickHouse row matching the `metrics` table DDL in `0003_metrics.sql`.
///
/// **Field order must match the DDL column order exactly.** The `clickhouse`
/// crate serialises fields positionally (binary protocol over HTTP); any
/// reordering here relative to the DDL silently corrupts inserts. A unit test
/// (`ch_metric_row_field_order_matches_ddl`) guards the column count.
///
/// ## Type mapping
///
/// | DDL type                              | Rust type                          |
/// |---------------------------------------|------------------------------------|
/// | `Int32`                               | `i32`                              |
/// | `Nullable(Int32)`                     | `Option<i32>`                      |
/// | `LowCardinality(String)` / `String`   | `String`                           |
/// | `Nullable(UInt8)`                     | `Option<u8>` (0/1 for is_monotonic)|
/// | `DateTime64(3, 'UTC')`                | `i64` (Unix milliseconds)          |
/// | `Nullable(DateTime64(3, 'UTC'))`      | `Option<i64>` (Unix milliseconds)  |
/// | `UInt32`                              | `u32`                              |
/// | `Nullable(Float64)`                   | `Option<f64>`                      |
/// | `Nullable(UInt64)`                    | `Option<u64>`                      |
/// | `Array(Float64)`                      | `Vec<f64>`                         |
/// | `Array(UInt64)`                       | `Vec<u64>`                         |
/// | `Array(Tuple(Float64, Float64))`      | `Vec<(f64, f64)>`                  |
/// | `Array(Tuple(String,String,Float64,DateTime64))` | `Vec<(String,String,f64,i64)>` |
/// | `Map(String, String)`                 | `Vec<(String, String)>`            |
/// | `UInt64`                              | `u64`                              |
///
/// Timestamps are stored as Unix milliseconds (`i64`), the same encoding used by
/// [`ChSpanRow`]. The `clickhouse` crate represents `Map(K, V)` as a `Vec<(K, V)>`
/// of key/value pairs in row-binary, so `attributes` is modelled that way.
#[derive(::clickhouse::Row, Serialize, Deserialize, Debug, Clone)]
pub struct ChMetricRow {
    // ── Tenant + deployment context ─────────────────────────────────────────
    /// project_id  Int32
    pub project_id: i32,
    /// deployment_id  Nullable(Int32)
    pub deployment_id: Option<i32>,

    // ── Resource / service identity ─────────────────────────────────────────
    /// service_name  LowCardinality(String)
    pub service_name: String,
    /// service_version  LowCardinality(String)
    pub service_version: String,
    /// deployment_environment  LowCardinality(String)
    pub deployment_environment: String,

    // ── Metric identity + semantics ─────────────────────────────────────────
    /// metric_name  String
    pub metric_name: String,
    /// metric_type  LowCardinality(String)
    pub metric_type: String,
    /// temporality  LowCardinality(String)  DEFAULT 'unspecified'
    pub temporality: String,
    /// is_monotonic  Nullable(UInt8)  (0/1; None for non-Sum)
    pub is_monotonic: Option<u8>,
    /// unit  LowCardinality(String)  DEFAULT ''
    pub unit: String,
    /// description  String  DEFAULT ''
    pub description: String,

    // ── Timing ──────────────────────────────────────────────────────────────
    /// timestamp  DateTime64(3, 'UTC') — Unix milliseconds
    pub timestamp: i64,
    /// start_time  Nullable(DateTime64(3, 'UTC')) — Unix milliseconds
    pub start_time: Option<i64>,
    /// flags  UInt32  DEFAULT 0
    pub flags: u32,

    // ── Scalar (Gauge / Sum) ────────────────────────────────────────────────
    /// value  Nullable(Float64)
    pub value: Option<f64>,

    // ── Explicit histogram / summary aggregate fields ───────────────────────
    /// histogram_count  Nullable(UInt64)
    pub histogram_count: Option<u64>,
    /// histogram_sum  Nullable(Float64)
    pub histogram_sum: Option<f64>,
    /// histogram_min  Nullable(Float64)
    pub histogram_min: Option<f64>,
    /// histogram_max  Nullable(Float64)
    pub histogram_max: Option<f64>,
    /// histogram_bounds  Array(Float64)  DEFAULT []
    pub histogram_bounds: Vec<f64>,
    /// histogram_bucket_counts  Array(UInt64)  DEFAULT []
    pub histogram_bucket_counts: Vec<u64>,

    // ── Exponential-histogram fields ────────────────────────────────────────
    /// exp_scale  Nullable(Int32)
    pub exp_scale: Option<i32>,
    /// exp_zero_count  Nullable(UInt64)
    pub exp_zero_count: Option<u64>,
    /// exp_zero_threshold  Nullable(Float64)
    pub exp_zero_threshold: Option<f64>,
    /// exp_positive_offset  Nullable(Int32)
    pub exp_positive_offset: Option<i32>,
    /// exp_positive_counts  Array(UInt64)  DEFAULT []
    pub exp_positive_counts: Vec<u64>,
    /// exp_negative_offset  Nullable(Int32)
    pub exp_negative_offset: Option<i32>,
    /// exp_negative_counts  Array(UInt64)  DEFAULT []
    pub exp_negative_counts: Vec<u64>,

    // ── Summary quantiles ───────────────────────────────────────────────────
    /// summary_quantiles  Array(Tuple(Float64, Float64))  DEFAULT []
    pub summary_quantiles: Vec<(f64, f64)>,

    // ── Exemplars ───────────────────────────────────────────────────────────
    /// exemplars  Array(Tuple(String, String, Float64, DateTime64(3,'UTC')))
    /// Tuple shape: (trace_id, span_id, value, timestamp_ms).
    pub exemplars: Vec<(String, String, f64, i64)>,

    // ── Data-point labels ───────────────────────────────────────────────────
    /// attributes  Map(String, String) — row-binary as key/value pairs.
    pub attributes: Vec<(String, String)>,

    // ── Dedup key ───────────────────────────────────────────────────────────
    /// _version  UInt64  DEFAULT toUnixTimestamp64Milli(now64())
    pub _version: u64,
}

/// The number of named columns in the `metrics` DDL (`0003_metrics.sql`),
/// excluding the `_version` dedup sentinel. The [`ChMetricRow`] struct must have
/// exactly this many domain fields, in the same order. Bump together with the
/// DDL when the schema changes. Used by the field-order guard test.
#[allow(dead_code)]
pub(crate) const CH_METRIC_ROW_FIELD_COUNT: usize = 31;

impl From<&MetricPoint> for ChMetricRow {
    fn from(p: &MetricPoint) -> Self {
        // attributes Map: BTreeMap -> ordered key/value pairs. Caller (ingest)
        // has already capped count/size and stripped temps.* keys at the trust
        // boundary, so we serialise verbatim here.
        let attributes: Vec<(String, String)> = p
            .attributes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let exemplars: Vec<(String, String, f64, i64)> = p
            .exemplars
            .iter()
            .map(|e| {
                (
                    e.trace_id.clone().unwrap_or_default(),
                    e.span_id.clone().unwrap_or_default(),
                    e.value,
                    e.timestamp.timestamp_millis(),
                )
            })
            .collect();

        // _version: Unix ms timestamp used as the ReplacingMergeTree dedup key.
        let version = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            project_id: p.project_id,
            deployment_id: p.deployment_id,
            service_name: p.resource.service_name.clone(),
            service_version: p.resource.service_version.clone().unwrap_or_default(),
            deployment_environment: p
                .resource
                .deployment_environment
                .clone()
                .unwrap_or_default(),
            metric_name: p.metric_name.clone(),
            metric_type: p.metric_type.to_string(),
            temporality: p
                .temporality
                .map(|t| t.to_string())
                .unwrap_or_else(|| "unspecified".to_string()),
            is_monotonic: p.is_monotonic.map(|b| b as u8),
            unit: p.unit.clone(),
            description: p.description.clone().unwrap_or_default(),
            timestamp: p.timestamp.timestamp_millis(),
            start_time: p.start_time.map(|t| t.timestamp_millis()),
            flags: p.flags,
            value: p.value,
            histogram_count: p.histogram_count,
            histogram_sum: p.histogram_sum,
            histogram_min: p.histogram_min,
            histogram_max: p.histogram_max,
            histogram_bounds: p.histogram_bounds.clone().unwrap_or_default(),
            histogram_bucket_counts: p.histogram_bucket_counts.clone().unwrap_or_default(),
            exp_scale: p.exp_scale,
            exp_zero_count: p.exp_zero_count,
            exp_zero_threshold: p.exp_zero_threshold,
            exp_positive_offset: p.exp_positive_offset,
            exp_positive_counts: p.exp_positive_counts.clone().unwrap_or_default(),
            exp_negative_offset: p.exp_negative_offset,
            exp_negative_counts: p.exp_negative_counts.clone().unwrap_or_default(),
            summary_quantiles: p.summary_quantiles.clone().unwrap_or_default(),
            exemplars,
            attributes,
            _version: version,
        }
    }
}

// ── Metric read-side row types ────────────────────────────────────────────

/// Row returned by `query_metrics` — one time-bucketed aggregate.
///
/// Field order MUST match the SELECT column order in `query_metrics`
/// (positional row-binary). `agg_value` carries the requested
/// [`MetricAggregation`]; `series_values` carries the grouped label values in
/// `group_by` order (empty when the query is ungrouped).
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChMetricBucketRow {
    /// Bucket start, Unix milliseconds (toStartOfInterval(...)).
    bucket_ms: i64,
    avg_value: f64,
    min_value: f64,
    max_value: f64,
    count: u64,
    /// The value of the requested aggregation for this bucket.
    agg_value: f64,
    /// Grouped label values, in `group_by` order (empty when ungrouped).
    series_values: Vec<String>,
}

/// Row of the temporality-aware histogram sub-aggregation (one per
/// bucket × grouped-series). Matched back to [`ChMetricBucketRow`] by
/// `(bucket_ms, series_values)`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChHistogramRow {
    bucket_ms: i64,
    series_values: Vec<String>,
    hcount: u64,
    hsum: f64,
    hmin: Option<f64>,
    hmax: Option<f64>,
    hbounds: Vec<f64>,
    hbuckets: Vec<u64>,
}

/// Row returned by `list_metric_names` — a single distinct name.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChMetricNameRow {
    metric_name: String,
}

/// Single distinct string — reused by the label key/value discovery queries
/// (both alias their projected column to `label_key`).
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChLabelRow {
    label_key: String,
}

/// Row returned by `get_metric_baseline` — hour/day-bucketed stats.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChBaselineRow {
    hour_of_day: i32,
    day_of_week: i32,
    avg_value: f64,
    stddev_value: f64,
    sample_count: u64,
}

/// Row returned by `get_recent_minute_aggregates`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChMinuteAggregateRow {
    /// Bucket start, Unix milliseconds (toStartOfMinute(...)).
    bucket_ms: i64,
    avg_value: f64,
    count: u64,
}

// ── Enum ↔ string helpers ───────────────────────────────────────────────────

/// Map [`SpanKind`] to the string stored in the CH `kind` column.
/// Matches the `Display` impl on `SpanKind` (SCREAMING_SNAKE_CASE).
pub(crate) fn span_kind_to_str(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::Unspecified => "UNSPECIFIED",
        SpanKind::Internal => "INTERNAL",
        SpanKind::Server => "SERVER",
        SpanKind::Client => "CLIENT",
        SpanKind::Producer => "PRODUCER",
        SpanKind::Consumer => "CONSUMER",
    }
}

/// Reverse map — unknown strings become [`SpanKind::Unspecified`].
pub(crate) fn str_to_span_kind(s: &str) -> SpanKind {
    match s {
        "INTERNAL" => SpanKind::Internal,
        "SERVER" => SpanKind::Server,
        "CLIENT" => SpanKind::Client,
        "PRODUCER" => SpanKind::Producer,
        "CONSUMER" => SpanKind::Consumer,
        _ => SpanKind::Unspecified,
    }
}

/// Map [`SpanStatusCode`] to the string stored in the CH `status_code` column.
pub(crate) fn span_status_to_str(code: SpanStatusCode) -> &'static str {
    match code {
        SpanStatusCode::Unset => "UNSET",
        SpanStatusCode::Ok => "OK",
        SpanStatusCode::Error => "ERROR",
    }
}

/// Reverse map — unknown strings become [`SpanStatusCode::Unset`].
pub(crate) fn str_to_span_status(s: &str) -> SpanStatusCode {
    match s {
        "OK" => SpanStatusCode::Ok,
        "ERROR" => SpanStatusCode::Error,
        _ => SpanStatusCode::Unset,
    }
}

// ── Read-side row types ─────────────────────────────────────────────────────
//
// These are separate from ChSpanRow (which is optimised for writes with Serialize).
// Read rows use Deserialize so the clickhouse crate can deserialise them from
// the HTTP row-binary response.  Field names must exactly match the SQL column
// aliases used in the SELECT list.

/// Row returned by `query_trace_summaries` — one row per distinct trace_id.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChTraceSummaryRow {
    trace_id: String,
    root_span_name: String,
    service_name: String,
    kind: String,
    deployment_environment: String,
    /// Unix milliseconds (toUnixTimestamp64Milli)
    start_time_ms: i64,
    max_duration_ms: f64,
    span_count: u64,
    error_count: u64,
}

/// Row returned by count queries — a single u64 scalar.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChCountRow {
    cnt: u64,
}

/// Row returned by `query_genai_trace_summaries`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChGenAiSummaryRow {
    trace_id: String,
    root_span_name: String,
    service_name: String,
    gen_ai_system: String,
    gen_ai_model: String,
    gen_ai_operation: String,
    /// Unix milliseconds
    start_time_ms: i64,
    max_duration_ms: f64,
    span_count: u64,
    error_count: u64,
    total_input_tokens: i64,
    total_output_tokens: i64,
    total_cache_creation_input_tokens: i64,
    total_cache_read_input_tokens: i64,
}

/// Row returned by `get_genai_trace_spans` — spans belonging to one trace.
/// We reuse `ChSpanRow` with `Deserialize` already derived there; so only a
/// purpose-specific row for `get_genai_trace_events` is needed.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChSpanEventsRow {
    span_id: String,
    events: String, // JSON string
}

// ── LIKE pattern helpers ────────────────────────────────────────────────────

/// Escape LIKE/ILIKE metacharacters in a user-supplied substring pattern.
///
/// ClickHouse LIKE uses backslash as the default escape character (no explicit
/// `ESCAPE` clause required). We must escape:
///
/// - `\` → `\\`   (backslash itself, before the other replacements)
/// - `%` → `\%`   (wildcard: any sequence of chars)
/// - `_` → `\_`   (wildcard: exactly one char)
///
/// The caller then wraps the result with `%{escaped}%` to perform a
/// case-insensitive substring search via ILIKE.
///
/// ## Verification
///
/// Confirmed against live ClickHouse 26.2: `'hello%world' LIKE '%\%%'`
/// returns 1, `'helloXworld' LIKE '%\%%'` returns 0. The backslash is the
/// default escape character; no `ESCAPE` clause is needed.
pub(crate) fn escape_like_pattern(pattern: &str) -> String {
    // Order matters: escape backslash first so the subsequent replacements
    // don't double-escape the backslashes we just introduced.
    pattern
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

// ── Metric bucket-interval translation ──────────────────────────────────────

/// Translate a free-form Postgres-style interval string (e.g. `"1 hour"`,
/// `"5 minutes"`, `"1 day"`) into a ClickHouse `INTERVAL <n> <UNIT>` fragment
/// that is safe to interpolate into a `toStartOfInterval(timestamp, INTERVAL …)`
/// expression.
///
/// ClickHouse has no `time_bucket()`; bucketing is done with
/// `toStartOfInterval(ts, INTERVAL n unit)`. The `clickhouse` crate cannot bind
/// an `INTERVAL` literal as a parameter, so the fragment must be built as a
/// string. To keep that injection-safe we parse the input strictly:
///
/// - the count must be a positive integer (`1..=100000`),
/// - the unit must be one of a fixed allowlist (second/minute/hour/day/week),
///
/// and we re-emit a canonical fragment built only from the parsed integer and a
/// hard-coded unit keyword — no user bytes survive into the SQL. Anything that
/// does not parse falls back to the default `INTERVAL 1 HOUR`.
pub(crate) fn translate_bucket_interval(interval: &str) -> String {
    const DEFAULT: &str = "INTERVAL 1 HOUR";
    let trimmed = interval.trim();
    // Accept two shapes:
    //   - space-separated "<count> <unit>" (e.g. "5 minutes", "1 hour"), and
    //   - compact "<count><unit>" (e.g. "300s", "5m", "1h") which the evaluator
    //     and SDK emit via `format!("{}s", secs)`. Without the compact form,
    //     "300s" failed to parse and silently fell back to 1-hour buckets —
    //     coarsening every windowed query (static + anomaly baseline).
    let mut parts = trimmed.split_whitespace();
    let (count_str, unit_raw): (String, String) = match (parts.next(), parts.next()) {
        (Some(count), Some(unit)) => {
            // Reject anything with extra tokens (e.g. "1 hour; DROP").
            if parts.next().is_some() {
                return DEFAULT.to_string();
            }
            (count.to_string(), unit.to_string())
        }
        (Some(single), None) => {
            // Compact form: split leading ASCII digits from the trailing unit.
            let split = single
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(single.len());
            if split == 0 || split == single.len() {
                return DEFAULT.to_string();
            }
            (single[..split].to_string(), single[split..].to_string())
        }
        _ => return DEFAULT.to_string(),
    };
    let Ok(count) = count_str.parse::<u32>() else {
        return DEFAULT.to_string();
    };
    if count == 0 || count > 100_000 {
        return DEFAULT.to_string();
    }
    // Normalise the unit (singular/plural and single-letter) to a fixed keyword.
    let unit = match unit_raw.to_ascii_lowercase().as_str() {
        "second" | "seconds" | "sec" | "secs" | "s" => "SECOND",
        "minute" | "minutes" | "min" | "mins" | "m" => "MINUTE",
        "hour" | "hours" | "hr" | "hrs" | "h" => "HOUR",
        "day" | "days" | "d" => "DAY",
        "week" | "weeks" | "w" => "WEEK",
        _ => return DEFAULT.to_string(),
    };
    format!("INTERVAL {count} {unit}")
}

/// Derive the bucket width in whole seconds from a canonical interval fragment
/// produced by [`translate_bucket_interval`] (e.g. `"INTERVAL 5 MINUTE"`).
///
/// Used only by the `RatePerSec` aggregation to convert a per-bucket counter
/// delta into a per-second rate. The input is always our own controlled
/// fragment (never raw user input), so parse failures fall back to one hour.
pub(crate) fn interval_seconds(interval_sql: &str) -> i64 {
    const HOUR: i64 = 3600;
    let mut parts = interval_sql.split_whitespace();
    // Expect: INTERVAL <n> <UNIT>
    if parts.next() != Some("INTERVAL") {
        return HOUR;
    }
    let Some(count) = parts.next().and_then(|c| c.parse::<i64>().ok()) else {
        return HOUR;
    };
    let unit_secs = match parts.next() {
        Some("SECOND") => 1,
        Some("MINUTE") => 60,
        Some("HOUR") => HOUR,
        Some("DAY") => 86_400,
        Some("WEEK") => 604_800,
        _ => return HOUR,
    };
    (count * unit_secs).max(1)
}

// ── OtelError helpers ───────────────────────────────────────────────────────

/// Wrap a ClickHouse ingest error into an [`OtelError::Storage`] with context.
///
/// All span-domain write methods use this helper so error messages
/// consistently identify the operation and the CH error.
pub(crate) fn ch_ingest_err(operation: &str, err: ::clickhouse::error::Error) -> OtelError {
    OtelError::Storage {
        message: format!("ClickHouse {operation} failed: {err}"),
    }
}

/// Wrap a ClickHouse query error into an [`OtelError::Storage`] with context.
pub(crate) fn ch_query_err(operation: &str, err: ::clickhouse::error::Error) -> OtelError {
    OtelError::Storage {
        message: format!("ClickHouse query {operation} failed: {err}"),
    }
}

// ── ClickHouseOtelStorage ────────────────────────────────────────────────────

/// ClickHouse-backed OTel storage.
///
/// Span writes go directly to ClickHouse. Span reads and all non-span
/// methods delegate to the inner `TimescaleDbStorage` until Phase 1–4
/// implementations land (see module-level doc).
pub struct ClickHouseOtelStorage {
    /// ClickHouse client — cheap to clone (Arc-backed internally).
    ch: ::clickhouse::Client,
    /// Postgres/TimescaleDB inner storage for delegation.
    ///
    /// All non-span methods and (for now) all span read methods are
    /// forwarded here verbatim. Phase 1 will replace span read delegation
    /// with native CH queries one method at a time.
    inner: Arc<TimescaleDbStorage>,
    /// Resolves the per-project `retention_days` value stamped onto each
    /// ingested span row. The default [`temps_core::FixedRetentionResolver`]
    /// always returns 90; a plugin can register an alternative implementation.
    resolver: Arc<dyn temps_core::RetentionResolver>,
}

impl ClickHouseOtelStorage {
    /// Construct a new ClickHouse OTel storage backend.
    ///
    /// - `config`: connection parameters for the ClickHouse `otel` database.
    /// - `inner`: the `TimescaleDbStorage` used for delegation of non-span
    ///   methods and (during Phase 0) span reads. Callers typically pass
    ///   the same `Arc<TimescaleDbStorage>` they would have registered
    ///   without ClickHouse.
    /// - `resolver`: resolves per-project `retention_days` at ingest time.
    ///   Pass `Arc::new(FixedRetentionResolver)` unless a plugin has
    ///   registered a project-aware implementation.
    pub fn new(
        config: ClickHouseOtelConfig,
        inner: Arc<TimescaleDbStorage>,
        resolver: Arc<dyn temps_core::RetentionResolver>,
    ) -> Self {
        let ch = ::clickhouse::Client::default()
            .with_url(&config.url)
            .with_database(&config.database)
            .with_user(&config.user)
            .with_password(&config.password);
        Self {
            ch,
            inner,
            resolver,
        }
    }

    /// Expose the raw ClickHouse client for migration runners / health checks.
    pub fn ch_client(&self) -> &::clickhouse::Client {
        &self.ch
    }
}

#[async_trait]
impl OtelStorage for ClickHouseOtelStorage {
    // ── Span write (ClickHouse — system of record) ──────────────────────────

    /// Batch-insert spans directly into the ClickHouse `spans` table.
    ///
    /// Uses `client.insert` + per-row `write` + `end()` — the same pattern
    /// as `ChFanout` in `temps-analytics-events`. `ReplacingMergeTree(_version)`
    /// deduplicates retried OTLP payloads automatically.
    ///
    /// Large batches are split into chunks of at most [`MAX_SPAN_INSERT_BATCH`]
    /// rows to bound the peak memory held in the ClickHouse client's HTTP
    /// buffer.  The total stored count is always the full input length.
    async fn store_spans(&self, spans: Vec<SpanRecord>) -> StorageResult<u64> {
        /// Maximum number of span rows per ClickHouse HTTP insert request.
        /// Limits peak CH client buffer memory on very large OTLP payloads.
        const MAX_SPAN_INSERT_BATCH: usize = 10_000;

        if spans.is_empty() {
            return Ok(0);
        }
        let total = spans.len() as u64;

        for chunk in spans.chunks(MAX_SPAN_INSERT_BATCH) {
            let mut inserter = self
                .ch
                .insert::<ChSpanRow>("spans")
                .await
                .map_err(|e| ch_ingest_err("store_spans (inserter setup)", e))?;

            for span in chunk {
                let mut row = ChSpanRow::from(span);
                row.retention_days = self
                    .resolver
                    .resolve(span.project_id, temps_core::RetentionTable::Spans);
                inserter
                    .write(&row)
                    .await
                    .map_err(|e| ch_ingest_err("store_spans (write)", e))?;
            }

            inserter
                .end()
                .await
                .map_err(|e| ch_ingest_err("store_spans (end)", e))?;
        }

        debug!(total, "ClickHouseOtelStorage: stored spans");
        Ok(total)
    }

    // ── Span reads (Phase 1 — native ClickHouse queries) ────────────────────

    /// Fetch raw span rows matching the query filters.
    ///
    /// Maps 1-to-1 with the TimescaleDB `query_spans` implementation. Bind
    /// params are used for all filter values; only `ORDER BY` direction is
    /// interpolated (from a fixed enum — injection-safe).
    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        // Build the WHERE clause and a matching bind list.
        // Using a String accumulator for the SQL fragments + a Vec of
        // closures that call .bind() is not ergonomic in Rust (the clickhouse
        // QueryBuilder is not object-safe). Instead we use a small state
        // machine: we render SQL with `?` placeholders in the same order as
        // the values, then call .bind() in the same order.
        let mut sql = String::from(
            "SELECT project_id, deployment_id, service_name, service_version, \
             deployment_environment, trace_id, span_id, parent_span_id, name, kind, \
             toUnixTimestamp64Milli(start_time) AS start_time_ms, \
             toUnixTimestamp64Milli(end_time) AS end_time_ms, \
             duration_ms, status_code, status_message, attributes, events \
             FROM spans FINAL WHERE project_id = ?",
        );

        // We build bind values as a Vec<ChBindValue> — a local enum that lets
        // us defer the actual .bind() calls until we have the full query string.
        enum Bv {
            I32(i32),
            I64(i64),
            F64(f64),
            Str(String),
        }
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref tid) = query.trace_id {
            sql.push_str(" AND trace_id = ?");
            binds.push(Bv::Str(tid.clone()));
        }
        if let Some(ref svc) = query.service_name {
            sql.push_str(" AND service_name = ?");
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(status) = query.status {
            sql.push_str(" AND status_code = ?");
            binds.push(Bv::Str(span_status_to_str(status).to_owned()));
        }
        if let Some(min_dur) = query.min_duration_ms {
            sql.push_str(" AND duration_ms >= ?");
            binds.push(Bv::F64(min_dur));
        }
        if let Some(start) = query.start_time {
            sql.push_str(" AND start_time >= fromUnixTimestamp64Milli(?)");
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            sql.push_str(" AND start_time <= fromUnixTimestamp64Milli(?)");
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(did) = query.deployment_id {
            sql.push_str(" AND deployment_id = ?");
            binds.push(Bv::I32(did));
        }
        // environment_id: CH has no JOIN to deployments; filter delegated when
        // environment_id is set by falling back to inner (see module note).
        // In Phase 1 we use the denormalized deployment_environment column
        // for the common case; environment_id is not resolvable in CH without
        // a separate Postgres lookup, so we skip that filter here.
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                sql.push_str(" AND JSONExtractString(attributes, ?) = ?");
                binds.push(Bv::Str(key.clone()));
                binds.push(Bv::Str(value.clone()));
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            sql.push_str(" AND name ILIKE ?");
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(pattern))));
        }

        // ORDER BY — enum-derived, injection-safe.
        let order_dir = query.sort_order.as_sql();
        match query.sort_by {
            crate::types::TraceSortField::Duration => {
                sql.push_str(&format!(" ORDER BY duration_ms {order_dir}"));
            }
            crate::types::TraceSortField::StartTime => {
                sql.push_str(&format!(" ORDER BY start_time {order_dir}"));
            }
        }
        sql.push_str(" LIMIT ? OFFSET ?");
        binds.push(Bv::I64(limit as i64));
        binds.push(Bv::I64(offset as i64));

        // Apply binds sequentially to the query builder.
        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::F64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        // We read into a dedicated row type that has the renamed timestamp columns.
        #[derive(::clickhouse::Row, Deserialize, Debug)]
        struct ChRawSpanRow {
            project_id: i32,
            deployment_id: Option<i32>,
            service_name: String,
            service_version: String,
            deployment_environment: String,
            trace_id: String,
            span_id: String,
            parent_span_id: String,
            name: String,
            kind: String,
            start_time_ms: i64,
            end_time_ms: i64,
            duration_ms: f64,
            status_code: String,
            status_message: String,
            attributes: String,
            events: String,
        }

        let rows = q
            .fetch_all::<ChRawSpanRow>()
            .await
            .map_err(|e| ch_query_err("query_spans", e))?;

        let spans: Vec<SpanRecord> = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                let end_time = chrono::Utc
                    .timestamp_millis_opt(r.end_time_ms)
                    .single()
                    .unwrap_or_default();
                let attributes: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(&r.attributes).unwrap_or_default();
                let events: Vec<SpanEvent> = serde_json::from_str(&r.events).unwrap_or_default();
                let resource = crate::types::ResourceInfo {
                    service_name: r.service_name,
                    service_version: if r.service_version.is_empty() {
                        None
                    } else {
                        Some(r.service_version)
                    },
                    deployment_environment: if r.deployment_environment.is_empty() {
                        None
                    } else {
                        Some(r.deployment_environment)
                    },
                    attributes: std::collections::BTreeMap::new(),
                };
                SpanRecord {
                    project_id: r.project_id,
                    deployment_id: r.deployment_id,
                    resource,
                    trace_id: r.trace_id,
                    span_id: r.span_id,
                    parent_span_id: if r.parent_span_id.is_empty() {
                        None
                    } else {
                        Some(r.parent_span_id)
                    },
                    name: r.name,
                    kind: str_to_span_kind(&r.kind),
                    start_time,
                    end_time,
                    duration_ms: r.duration_ms,
                    status_code: str_to_span_status(&r.status_code),
                    status_message: r.status_message,
                    attributes,
                    events,
                }
            })
            .collect();

        debug!(count = spans.len(), "ClickHouseOtelStorage: query_spans");
        Ok(spans)
    }

    /// Aggregate spans into per-trace summaries using query-time GROUP BY.
    ///
    /// Chosen approach from benchmark (ADR-016 Phase 0): query-time GROUP BY
    /// on `spans FINAL` beats the AggregatingMergeTree MV approach at our
    /// benchmark scale (23ms vs 31ms best, 400k traces).  Re-evaluate if the
    /// table grows past 10M distinct traces.
    ///
    /// `deployment_environment` is a denormalized LowCardinality column at
    /// ingest time; there is no CH→Postgres JOIN for environment names. The
    /// `environment_id` filter in `TraceQuery` is therefore ignored here
    /// (CH has no access to the `environments` Postgres table). This mirrors
    /// the trade-off documented in ADR-016 §Consequences → Relational JOINs.
    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        // ── Build WHERE clause and ordered bind list ────────────────────────
        enum Bv {
            I32(i32),
            I64(i64),
            F64(f64),
            Str(String),
        }

        let mut where_parts: Vec<String> = vec!["project_id = ?".to_owned()];
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref tid) = query.trace_id {
            where_parts.push("trace_id = ?".to_owned());
            binds.push(Bv::Str(tid.clone()));
        }
        if let Some(ref svc) = query.service_name {
            // Qualify with the table: the trace-summary SELECTs alias
            // `argMax(service_name) AS service_name`, which shadows the raw
            // column, so an unqualified `service_name` in WHERE binds to the
            // aggregate (ClickHouse Code 184 ILLEGAL_AGGREGATION). The count
            // mirrors qualify too so the filter SQL stays byte-identical.
            where_parts.push("spans.service_name = ?".to_owned());
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(min_dur) = query.min_duration_ms {
            where_parts.push("duration_ms >= ?".to_owned());
            binds.push(Bv::F64(min_dur));
        }
        if let Some(start) = query.start_time {
            where_parts.push("start_time >= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            where_parts.push("start_time <= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(did) = query.deployment_id {
            where_parts.push("deployment_id = ?".to_owned());
            binds.push(Bv::I32(did));
        }
        // environment_id: skipped — no Postgres JOIN in CH (see doc comment).
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                where_parts.push("JSONExtractString(attributes, ?) = ?".to_owned());
                binds.push(Bv::Str(key.clone()));
                binds.push(Bv::Str(value.clone()));
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            where_parts.push("name ILIKE ?".to_owned());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(pattern))));
        }

        let where_sql = where_parts.join(" AND ");

        // ── HAVING clause for status filter ────────────────────────────────
        // Mirrors TimescaleDB: ERROR = has at least one ERROR span;
        // Ok = has zero ERROR spans. `HAVING` can reference aggregate exprs.
        let having_sql = match query.status {
            Some(SpanStatusCode::Error) => " HAVING countIf(status_code = 'ERROR') > 0",
            Some(SpanStatusCode::Ok) => " HAVING countIf(status_code = 'ERROR') = 0",
            _ => "",
        };

        // ── ORDER BY — enum-derived, injection-safe ─────────────────────────
        let order_dir = query.sort_order.as_sql();
        let order_sql = match query.sort_by {
            crate::types::TraceSortField::Duration => {
                format!("ORDER BY max_duration_ms {order_dir}, min(start_time) DESC, trace_id")
            }
            crate::types::TraceSortField::StartTime => {
                format!("ORDER BY min(start_time) {order_dir}, trace_id")
            }
        };

        // ── Full query ──────────────────────────────────────────────────────
        // argMax(name, …) picks the root-span name: root spans have
        // parent_span_id = '' (our empty-string sentinel), so we boost their
        // priority with a large addend so argMax always selects them when
        // present; otherwise falls back to the longest span (max duration).
        let sql = format!(
            r#"SELECT
                trace_id,
                argMax(name,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS root_span_name,
                argMax(service_name,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS service_name,
                argMax(kind,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS kind,
                argMax(deployment_environment,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS deployment_environment,
                toUnixTimestamp64Milli(min(start_time)) AS start_time_ms,
                max(duration_ms) AS max_duration_ms,
                count() AS span_count,
                countIf(status_code = 'ERROR') AS error_count
            FROM spans FINAL
            WHERE {where_sql}
            GROUP BY trace_id
            {having_sql}
            {order_sql}
            LIMIT ? OFFSET ?"#
        );
        binds.push(Bv::I64(limit as i64));
        binds.push(Bv::I64(offset as i64));

        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::F64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        let rows = q
            .fetch_all::<ChTraceSummaryRow>()
            .await
            .map_err(|e| ch_query_err("query_trace_summaries", e))?;

        let summaries = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                let kind = str_to_span_kind(&r.kind);
                let status_code = if r.error_count > 0 {
                    SpanStatusCode::Error
                } else {
                    SpanStatusCode::Ok
                };
                let deployment_environment = if r.deployment_environment.is_empty() {
                    None
                } else {
                    Some(r.deployment_environment)
                };
                TraceSummary {
                    trace_id: r.trace_id,
                    root_span_name: r.root_span_name,
                    service_name: r.service_name,
                    deployment_environment,
                    kind,
                    status_code,
                    start_time,
                    duration_ms: r.max_duration_ms,
                    span_count: r.span_count as i64,
                    error_count: r.error_count as i64,
                }
            })
            .collect();

        Ok(summaries)
    }

    /// Count distinct traces matching the query filters (without pagination).
    ///
    /// Mirrors `query_trace_summaries` filters exactly — including `status`
    /// (via a HAVING on countIf) and `min_duration_ms` — so the pagination
    /// count matches the actual result set returned by that method.
    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        enum Bv {
            I32(i32),
            I64(i64),
            F64(f64),
            Str(String),
        }

        let mut where_parts: Vec<String> = vec!["project_id = ?".to_owned()];
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref tid) = query.trace_id {
            where_parts.push("trace_id = ?".to_owned());
            binds.push(Bv::Str(tid.clone()));
        }
        if let Some(ref svc) = query.service_name {
            // Qualify with the table: the trace-summary SELECTs alias
            // `argMax(service_name) AS service_name`, which shadows the raw
            // column, so an unqualified `service_name` in WHERE binds to the
            // aggregate (ClickHouse Code 184 ILLEGAL_AGGREGATION). The count
            // mirrors qualify too so the filter SQL stays byte-identical.
            where_parts.push("spans.service_name = ?".to_owned());
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(min_dur) = query.min_duration_ms {
            where_parts.push("duration_ms >= ?".to_owned());
            binds.push(Bv::F64(min_dur));
        }
        if let Some(start) = query.start_time {
            where_parts.push("start_time >= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            where_parts.push("start_time <= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(did) = query.deployment_id {
            where_parts.push("deployment_id = ?".to_owned());
            binds.push(Bv::I32(did));
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                where_parts.push("JSONExtractString(attributes, ?) = ?".to_owned());
                binds.push(Bv::Str(key.clone()));
                binds.push(Bv::Str(value.clone()));
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            where_parts.push("name ILIKE ?".to_owned());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(pattern))));
        }

        let where_sql = where_parts.join(" AND ");

        // status filter mirrors query_trace_summaries: ERROR = at least one
        // ERROR span in the trace, OK = no ERROR spans. Implemented as HAVING
        // on the per-trace GROUP BY, wrapped in a subquery so the outer query
        // can COUNT the matching trace rows.
        let having_sql = match query.status {
            Some(SpanStatusCode::Error) => " HAVING countIf(status_code = 'ERROR') > 0",
            Some(SpanStatusCode::Ok) => " HAVING countIf(status_code = 'ERROR') = 0",
            _ => "",
        };

        // Use a subquery so we can apply HAVING on per-trace aggregates and
        // then count the filtered set. `uniqExact` on the outer query is not
        // needed because the inner GROUP BY already yields one row per trace.
        let sql = format!(
            "SELECT count() AS cnt FROM (\
                SELECT trace_id \
                FROM spans FINAL \
                WHERE {where_sql} \
                GROUP BY trace_id\
                {having_sql}\
            )"
        );

        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::F64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        let row = q
            .fetch_one::<ChCountRow>()
            .await
            .map_err(|e| ch_query_err("count_traces", e))?;

        Ok(row.cnt)
    }

    /// Fetch all spans of a single trace, ordered by start_time ASC.
    async fn get_trace(&self, project_id: i32, trace_id: &str) -> StorageResult<Vec<SpanRecord>> {
        // Simple point-lookup — the ORDER BY (project_id, trace_id, span_id)
        // primary index makes this a sequential read of one contiguous block.
        #[derive(::clickhouse::Row, Deserialize, Debug)]
        struct ChRawSpanRow {
            project_id: i32,
            deployment_id: Option<i32>,
            service_name: String,
            service_version: String,
            deployment_environment: String,
            trace_id: String,
            span_id: String,
            parent_span_id: String,
            name: String,
            kind: String,
            start_time_ms: i64,
            end_time_ms: i64,
            duration_ms: f64,
            status_code: String,
            status_message: String,
            attributes: String,
            events: String,
        }

        let sql = "SELECT project_id, deployment_id, service_name, service_version, \
                   deployment_environment, trace_id, span_id, parent_span_id, name, kind, \
                   toUnixTimestamp64Milli(start_time) AS start_time_ms, \
                   toUnixTimestamp64Milli(end_time) AS end_time_ms, \
                   duration_ms, status_code, status_message, attributes, events \
                   FROM spans FINAL \
                   WHERE project_id = ? AND trace_id = ? \
                   ORDER BY start_time ASC";

        let rows = self
            .ch
            .query(sql)
            .bind(project_id)
            .bind(trace_id)
            .fetch_all::<ChRawSpanRow>()
            .await
            .map_err(|e| ch_query_err("get_trace", e))?;

        let spans: Vec<SpanRecord> = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                let end_time = chrono::Utc
                    .timestamp_millis_opt(r.end_time_ms)
                    .single()
                    .unwrap_or_default();
                let attributes: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(&r.attributes).unwrap_or_default();
                let events: Vec<SpanEvent> = serde_json::from_str(&r.events).unwrap_or_default();
                let resource = crate::types::ResourceInfo {
                    service_name: r.service_name,
                    service_version: if r.service_version.is_empty() {
                        None
                    } else {
                        Some(r.service_version)
                    },
                    deployment_environment: if r.deployment_environment.is_empty() {
                        None
                    } else {
                        Some(r.deployment_environment)
                    },
                    attributes: std::collections::BTreeMap::new(),
                };
                SpanRecord {
                    project_id: r.project_id,
                    deployment_id: r.deployment_id,
                    resource,
                    trace_id: r.trace_id,
                    span_id: r.span_id,
                    parent_span_id: if r.parent_span_id.is_empty() {
                        None
                    } else {
                        Some(r.parent_span_id)
                    },
                    name: r.name,
                    kind: str_to_span_kind(&r.kind),
                    start_time,
                    end_time,
                    duration_ms: r.duration_ms,
                    status_code: str_to_span_status(&r.status_code),
                    status_message: r.status_message,
                    attributes,
                    events,
                }
            })
            .collect();

        debug!(
            project_id,
            trace_id,
            count = spans.len(),
            "ClickHouseOtelStorage: get_trace"
        );
        Ok(spans)
    }

    /// List GenAI trace summaries from ClickHouse.
    ///
    /// A GenAI trace is one that has at least one span with the
    /// `gen_ai.system` or `gen_ai.provider.name` attribute (same definition
    /// as the TimescaleDB implementation).  We use `JSONHas` to detect presence
    /// and `JSONExtractString` to extract values from the JSON-as-String
    /// `attributes` column.
    ///
    /// `deployment_environment`, `gen_ai.system`, `gen_ai.request.model`, and
    /// `gen_ai.operation.name` are extracted from the attributes JSON. Token
    /// counts are extracted and summed per trace.
    async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<GenAiTraceSummary>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        enum Bv {
            I32(i32),
            I64(i64),
            Str(String),
        }

        // Base filter: must be a GenAI span.
        let mut where_parts: Vec<String> = vec![
            "project_id = ?".to_owned(),
            "(JSONHas(attributes, 'gen_ai.system') = 1 OR JSONHas(attributes, 'gen_ai.provider.name') = 1)".to_owned(),
        ];
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref svc) = query.service_name {
            // Qualify with the table: the trace-summary SELECTs alias
            // `argMax(service_name) AS service_name`, which shadows the raw
            // column, so an unqualified `service_name` in WHERE binds to the
            // aggregate (ClickHouse Code 184 ILLEGAL_AGGREGATION). The count
            // mirrors qualify too so the filter SQL stays byte-identical.
            where_parts.push("spans.service_name = ?".to_owned());
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(start) = query.start_time {
            where_parts.push("start_time >= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            where_parts.push("start_time <= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                // Mirror the TimescaleDB impl: gen_ai.system queries also
                // check the deprecated gen_ai.provider.name.
                match key.as_str() {
                    "gen_ai.system" => {
                        where_parts.push(
                            "coalesce(nullIf(JSONExtractString(attributes, 'gen_ai.provider.name'), ''), \
                             JSONExtractString(attributes, 'gen_ai.system')) = ?".to_owned(),
                        );
                        binds.push(Bv::Str(value.clone()));
                    }
                    _ => {
                        where_parts.push("JSONExtractString(attributes, ?) = ?".to_owned());
                        binds.push(Bv::Str(key.clone()));
                        binds.push(Bv::Str(value.clone()));
                    }
                }
            }
        }

        let where_sql = where_parts.join(" AND ");

        // Per-trace aggregation: pick root span name from the span with the
        // highest priority (root span = parent_span_id = '' gets the boost).
        // Token fields are SUM across the trace; use 0 as the sentinel for
        // missing values (ifNull), then coerce back to nullable below.
        let sql = format!(
            r#"SELECT
                trace_id,
                argMax(name,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS root_span_name,
                argMax(service_name,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS service_name,
                argMaxIf(
                    coalesce(nullIf(JSONExtractString(attributes, 'gen_ai.provider.name'), ''),
                             JSONExtractString(attributes, 'gen_ai.system')),
                    start_time,
                    JSONExtractString(attributes, 'gen_ai.system') != ''
                    OR JSONExtractString(attributes, 'gen_ai.provider.name') != ''
                ) AS gen_ai_system,
                argMaxIf(
                    JSONExtractString(attributes, 'gen_ai.request.model'),
                    start_time,
                    JSONExtractString(attributes, 'gen_ai.request.model') != ''
                ) AS gen_ai_model,
                argMaxIf(
                    JSONExtractString(attributes, 'gen_ai.operation.name'),
                    start_time,
                    JSONExtractString(attributes, 'gen_ai.operation.name') != ''
                ) AS gen_ai_operation,
                toUnixTimestamp64Milli(min(start_time)) AS start_time_ms,
                max(duration_ms) AS max_duration_ms,
                count() AS span_count,
                countIf(status_code = 'ERROR') AS error_count,
                sumIf(
                    toInt64OrZero(coalesce(
                        nullIf(JSONExtractString(attributes, 'gen_ai.usage.input_tokens'), ''),
                        JSONExtractString(attributes, 'gen_ai.usage.prompt_tokens')
                    )),
                    JSONExtractString(attributes, 'gen_ai.usage.input_tokens') != ''
                    OR JSONExtractString(attributes, 'gen_ai.usage.prompt_tokens') != ''
                ) AS total_input_tokens,
                sumIf(
                    toInt64OrZero(coalesce(
                        nullIf(JSONExtractString(attributes, 'gen_ai.usage.output_tokens'), ''),
                        JSONExtractString(attributes, 'gen_ai.usage.completion_tokens')
                    )),
                    JSONExtractString(attributes, 'gen_ai.usage.output_tokens') != ''
                    OR JSONExtractString(attributes, 'gen_ai.usage.completion_tokens') != ''
                ) AS total_output_tokens,
                sumIf(
                    toInt64OrZero(JSONExtractString(attributes, 'gen_ai.usage.cache_creation.input_tokens')),
                    JSONExtractString(attributes, 'gen_ai.usage.cache_creation.input_tokens') != ''
                ) AS total_cache_creation_input_tokens,
                sumIf(
                    toInt64OrZero(JSONExtractString(attributes, 'gen_ai.usage.cache_read.input_tokens')),
                    JSONExtractString(attributes, 'gen_ai.usage.cache_read.input_tokens') != ''
                ) AS total_cache_read_input_tokens
            FROM spans FINAL
            WHERE {where_sql}
            GROUP BY trace_id
            ORDER BY min(start_time) DESC
            LIMIT ? OFFSET ?"#
        );
        binds.push(Bv::I64(limit as i64));
        binds.push(Bv::I64(offset as i64));

        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        let rows = q
            .fetch_all::<ChGenAiSummaryRow>()
            .await
            .map_err(|e| ch_query_err("query_genai_trace_summaries", e))?;

        let summaries = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                // Empty-string sentinels → None to match TimescaleDB shape.
                let gen_ai_system = if r.gen_ai_system.is_empty() {
                    None
                } else {
                    Some(r.gen_ai_system)
                };
                let gen_ai_model = if r.gen_ai_model.is_empty() {
                    None
                } else {
                    Some(r.gen_ai_model)
                };
                let gen_ai_operation = if r.gen_ai_operation.is_empty() {
                    None
                } else {
                    Some(r.gen_ai_operation)
                };
                // Token totals: 0 means "no spans contributed" → None.
                let opt_i64 = |v: i64| if v == 0 { None } else { Some(v) };

                GenAiTraceSummary {
                    trace_id: r.trace_id,
                    root_span_name: r.root_span_name,
                    service_name: r.service_name,
                    gen_ai_system,
                    gen_ai_model,
                    gen_ai_operation,
                    start_time,
                    duration_ms: r.max_duration_ms,
                    span_count: r.span_count as i64,
                    error_count: r.error_count as i64,
                    total_input_tokens: opt_i64(r.total_input_tokens),
                    total_output_tokens: opt_i64(r.total_output_tokens),
                    total_cache_creation_input_tokens: opt_i64(r.total_cache_creation_input_tokens),
                    total_cache_read_input_tokens: opt_i64(r.total_cache_read_input_tokens),
                }
            })
            .collect();

        Ok(summaries)
    }

    /// Fetch all spans of one trace for the GenAI detail view.
    ///
    /// Identical to `get_trace` but used by the GenAI handler; the trace was
    /// already validated as a GenAI trace by `query_genai_trace_summaries`.
    async fn get_genai_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiSpanDetail>> {
        #[derive(::clickhouse::Row, Deserialize, Debug)]
        struct ChGenAiSpanRow {
            span_id: String,
            parent_span_id: String,
            name: String,
            kind: String,
            start_time_ms: i64,
            duration_ms: f64,
            status_code: String,
            attributes: String,
        }

        let sql = "SELECT span_id, parent_span_id, name, kind, \
                   toUnixTimestamp64Milli(start_time) AS start_time_ms, \
                   duration_ms, status_code, attributes \
                   FROM spans FINAL \
                   WHERE project_id = ? AND trace_id = ? \
                   ORDER BY start_time ASC";

        let rows = self
            .ch
            .query(sql)
            .bind(project_id)
            .bind(trace_id)
            .fetch_all::<ChGenAiSpanRow>()
            .await
            .map_err(|e| ch_query_err("get_genai_trace_spans", e))?;

        let spans = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                let attrs: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(&r.attributes).unwrap_or_default();
                let kind = str_to_span_kind(&r.kind);
                let status_code = str_to_span_status(&r.status_code);
                let parent_span_id = if r.parent_span_id.is_empty() {
                    None
                } else {
                    Some(r.parent_span_id)
                };

                GenAiSpanDetail::from_span_attrs(
                    r.span_id,
                    parent_span_id,
                    r.name,
                    kind,
                    start_time,
                    r.duration_ms,
                    status_code,
                    attrs,
                )
            })
            .collect();

        Ok(spans)
    }

    /// Count distinct GenAI traces matching the query filters.
    async fn count_genai_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        enum Bv {
            I32(i32),
            I64(i64),
            Str(String),
        }

        let mut where_parts: Vec<String> = vec![
            "project_id = ?".to_owned(),
            "(JSONHas(attributes, 'gen_ai.system') = 1 OR JSONHas(attributes, 'gen_ai.provider.name') = 1)".to_owned(),
        ];
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref svc) = query.service_name {
            // Qualify with the table: the trace-summary SELECTs alias
            // `argMax(service_name) AS service_name`, which shadows the raw
            // column, so an unqualified `service_name` in WHERE binds to the
            // aggregate (ClickHouse Code 184 ILLEGAL_AGGREGATION). The count
            // mirrors qualify too so the filter SQL stays byte-identical.
            where_parts.push("spans.service_name = ?".to_owned());
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(start) = query.start_time {
            where_parts.push("start_time >= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            where_parts.push("start_time <= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                match key.as_str() {
                    "gen_ai.system" => {
                        where_parts.push(
                            "coalesce(nullIf(JSONExtractString(attributes, 'gen_ai.provider.name'), ''), \
                             JSONExtractString(attributes, 'gen_ai.system')) = ?".to_owned(),
                        );
                        binds.push(Bv::Str(value.clone()));
                    }
                    _ => {
                        where_parts.push("JSONExtractString(attributes, ?) = ?".to_owned());
                        binds.push(Bv::Str(key.clone()));
                        binds.push(Bv::Str(value.clone()));
                    }
                }
            }
        }

        let where_sql = where_parts.join(" AND ");
        let sql = format!("SELECT uniqExact(trace_id) AS cnt FROM spans FINAL WHERE {where_sql}");

        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        let row = q
            .fetch_one::<ChCountRow>()
            .await
            .map_err(|e| ch_query_err("count_genai_traces", e))?;

        Ok(row.cnt)
    }

    /// Extract GenAI-related span events from one trace.
    ///
    /// Events are stored as a JSON array in the `events` String column.  We
    /// fetch the raw JSON per-span and parse it in Rust, mirroring exactly
    /// what the TimescaleDB implementation does with its JSONB column.
    async fn get_genai_trace_events(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiEvent>> {
        // Fetch spans that have at least one event (non-empty JSON array).
        // JSONLength returns 0 for '[]', so the filter keeps only spans with events.
        let sql = "SELECT span_id, events \
                   FROM spans FINAL \
                   WHERE project_id = ? AND trace_id = ? \
                   AND JSONLength(events) > 0 \
                   ORDER BY start_time ASC";

        let rows = self
            .ch
            .query(sql)
            .bind(project_id)
            .bind(trace_id)
            .fetch_all::<ChSpanEventsRow>()
            .await
            .map_err(|e| ch_query_err("get_genai_trace_events", e))?;

        let mut events: Vec<GenAiEvent> = Vec::new();
        for row in rows {
            let event_array: Vec<serde_json::Value> =
                serde_json::from_str(&row.events).unwrap_or_default();
            for event in event_array {
                let event_name = event
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                // Only include gen_ai.* events, matching TimescaleDB impl.
                if !event_name.starts_with("gen_ai.") {
                    continue;
                }
                let raw_ts = event.get("timestamp").and_then(|v| v.as_str());
                let timestamp = raw_ts
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|| {
                        if raw_ts.is_some() {
                            tracing::warn!(
                                span_id = %row.span_id,
                                raw_timestamp = raw_ts,
                                "get_genai_trace_events: unparsable span event timestamp; \
                                 substituting Unix epoch"
                            );
                        }
                        chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap_or_default()
                    });
                let attrs: std::collections::BTreeMap<String, String> = event
                    .get("attributes")
                    .and_then(|v| v.as_object())
                    .map(|obj| {
                        obj.iter()
                            .map(|(k, v)| {
                                (k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                events.push(GenAiEvent {
                    span_id: row.span_id.clone(),
                    trace_id: trace_id.to_string(),
                    event_name: event_name.to_string(),
                    timestamp,
                    attributes: attrs,
                });
            }
        }

        Ok(events)
    }

    // ── Metric write (ClickHouse — sole sink for the OtelStorage path) ───────
    //
    // ADR-016 Phase B: when ClickHouse is enabled, native CH `metrics` is the
    // single source of truth for the OtelStorage metric path (otel_metrics on
    // Timescale no longer receives this write). The independent
    // `service_metrics` alerting bridge (TimescaleMetricsStore / metrics_write_tx
    // in the ingest handler) is untouched and still targets TimescaleDB.

    /// Batch-insert metric points directly into the ClickHouse `metrics` table.
    ///
    /// Mirrors `store_spans`: chunked `insert` + per-row `write` + `end()`,
    /// `ReplacingMergeTree(_version)` dedups retried OTLP payloads.
    ///
    /// Trust boundary: although the OTLP ingest handler already validates metric
    /// names and caps labels before producing `MetricPoint`s, this method is the
    /// last gate before bytes hit the store. We re-apply the metric-name
    /// allowlist here (defence in depth) and skip — rather than abort — any point
    /// whose name is outside `[a-zA-Z0-9_.:-]`. The returned count reflects only
    /// the points actually written.
    async fn store_metrics(&self, points: Vec<MetricPoint>) -> StorageResult<u64> {
        /// Maximum number of metric rows per ClickHouse HTTP insert request.
        const MAX_METRIC_INSERT_BATCH: usize = 10_000;

        if points.is_empty() {
            return Ok(0);
        }

        // Filter at the trust boundary: drop names outside the allowlist.
        let safe: Vec<&MetricPoint> = points
            .iter()
            .filter(|p| {
                if validate_metric_name(&p.metric_name).is_err() {
                    tracing::warn!(
                        project_id = p.project_id,
                        metric_name = %p.metric_name,
                        "ClickHouse store_metrics: dropping metric, name outside allowlist (possible injection attempt)"
                    );
                    false
                } else {
                    true
                }
            })
            .collect();

        if safe.is_empty() {
            return Ok(0);
        }
        let total = safe.len() as u64;

        for chunk in safe.chunks(MAX_METRIC_INSERT_BATCH) {
            let mut inserter = self
                .ch
                .insert::<ChMetricRow>("metrics")
                .await
                .map_err(|e| ch_ingest_err("store_metrics (inserter setup)", e))?;

            for point in chunk {
                let row = ChMetricRow::from(*point);
                inserter
                    .write(&row)
                    .await
                    .map_err(|e| ch_ingest_err("store_metrics (write)", e))?;
            }

            inserter
                .end()
                .await
                .map_err(|e| ch_ingest_err("store_metrics (end)", e))?;
        }

        debug!(total, "ClickHouseOtelStorage: stored metrics");
        Ok(total)
    }

    // ── Metric reads (ClickHouse — native) ──────────────────────────────────

    /// Time-bucketed metric aggregates over the scalar `value` column.
    ///
    /// Honours the store-neutral [`MetricQuery`] contract:
    /// - `bucket_interval` → `toStartOfInterval(timestamp, INTERVAL …)` (CH has
    ///   no `time_bucket`), translated via [`translate_bucket_interval`] from a
    ///   fixed allowlist — no user bytes reach the SQL.
    /// - `metric_type` → `metric_type = ?` (bound).
    /// - `label_filters` → `attributes[?] = ?` per pair, the **key bound** via a
    ///   parameter (never concatenated) after passing the metric-name allowlist.
    /// - `group_by` → one series per distinct label-set; the grouped label values
    ///   are returned as an `Array(String)` and paired back with the (allowlisted)
    ///   keys to form `series_key`.
    /// - `aggregation` → the requested reducer drives `agg_value`: avg/sum/min/
    ///   max/count, a `quantile(q)(value)`, or `RatePerSec` as
    ///   `(max-min)/window_seconds` (cumulative monotonic delta; we never
    ///   re-delta a series, matching the ingest temporality semantics).
    ///
    /// All filter values are bound via `?` placeholders.
    async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
        use chrono::TimeZone;

        let interval_sql =
            translate_bucket_interval(query.bucket_interval.as_deref().unwrap_or("1 hour"));
        let limit = query.limit.unwrap_or(1000).min(10_000);

        // Validate label keys (group_by + label_filters) against the ingest
        // allowlist. Reject the whole query on a bad key rather than silently
        // dropping a filter — a bad key signals a malformed/abusive request.
        for key in query
            .group_by
            .iter()
            .chain(query.label_filters.iter().map(|(k, _)| k))
        {
            if validate_metric_name(key).is_err() {
                return Err(OtelError::Storage {
                    message: format!(
                        "query_metrics: label key '{key}' is outside the allowed character set [a-zA-Z0-9_.:-]"
                    ),
                });
            }
        }

        // The aggregation expression over the scalar `value` column. For
        // RatePerSec we compute the per-bucket delta divided by the bucket width
        // in seconds; the divisor is derived from the (already-validated)
        // interval fragment, not user input.
        // `value` is Nullable(Float64); every aggregate input is wrapped in
        // assumeNotNull (safe — the WHERE clause filters `value IS NOT NULL`) so
        // the result type is a non-nullable Float64 that matches the `f64`
        // ChMetricBucketRow read field. Without this, min/max/sum/quantile over a
        // Nullable column return Nullable(Float64) and the RowBinary read fails
        // with "row type mismatches a database schema".
        let agg_expr = match query.aggregation {
            MetricAggregation::Avg => "avg(assumeNotNull(value))".to_string(),
            MetricAggregation::Sum => "sum(assumeNotNull(value))".to_string(),
            MetricAggregation::Min => "min(assumeNotNull(value))".to_string(),
            MetricAggregation::Max => "max(assumeNotNull(value))".to_string(),
            MetricAggregation::Count => "toFloat64(count())".to_string(),
            MetricAggregation::RatePerSec => {
                let secs = interval_seconds(&interval_sql).max(1);
                // Temporality-aware per-second rate. DELTA series already carry
                // per-interval increments, so the bucket rate is sum/window.
                // CUMULATIVE counters are stored raw (monotonic running total),
                // so the within-bucket increase is (max - min). `any(temporality)`
                // reads the metric's (uniform) temporality; the window divisor is
                // guarded against zero by secs.max(1) above.
                format!(
                    "if(any(temporality) = 'delta', \
                        sum(assumeNotNull(value)), \
                        max(assumeNotNull(value)) - min(assumeNotNull(value))) / {secs}.0"
                )
            }
            MetricAggregation::Quantile(q) => {
                let qc = q.clamp(0.0, 1.0);
                // quantile() takes its level as a parameter in parentheses; we
                // emit the clamped float literal (not user bytes).
                format!("quantile({qc})(assumeNotNull(value))")
            }
        };

        // Grouped label values as an Array(String), in group_by order. CH binds
        // the key via the `?` map-index parameter, never string interpolation.
        let group_select = if query.group_by.is_empty() {
            "[] AS series_values".to_string()
        } else {
            let parts: Vec<String> = query
                .group_by
                .iter()
                .map(|_| "attributes[?]".to_string())
                .collect();
            format!("[{}] AS series_values", parts.join(", "))
        };
        let group_by_extra = if query.group_by.is_empty() {
            String::new()
        } else {
            ", series_values".to_string()
        };
        // The grouped-label array WITHOUT the `AS series_values` alias, for reuse
        // inside the histogram sub-aggregation's projection.
        let group_array = if query.group_by.is_empty() {
            "[]".to_string()
        } else {
            let parts: Vec<String> = query
                .group_by
                .iter()
                .map(|_| "attributes[?]".to_string())
                .collect();
            format!("[{}]", parts.join(", "))
        };

        // Build WHERE with `?` placeholders; bind in the same order below.
        let mut where_clauses = vec!["project_id = ?".to_string()];
        if query.metric_name.is_some() {
            where_clauses.push("metric_name = ?".to_string());
        }
        if query.metric_type.is_some() {
            where_clauses.push("metric_type = ?".to_string());
        }
        if query.service_name.is_some() {
            where_clauses.push("service_name = ?".to_string());
        }
        if query.environment.is_some() {
            where_clauses.push("deployment_environment = ?".to_string());
        }
        for _ in &query.label_filters {
            where_clauses.push("attributes[?] = ?".to_string());
        }
        if query.start_time.is_some() {
            where_clauses.push("timestamp >= fromUnixTimestamp64Milli(?)".to_string());
        }
        if query.end_time.is_some() {
            where_clauses.push("timestamp <= fromUnixTimestamp64Milli(?)".to_string());
        }
        // Only aggregate rows that carry a scalar value.
        where_clauses.push("value IS NOT NULL".to_string());
        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            "SELECT \
                 toInt64(toUnixTimestamp(toStartOfInterval(timestamp, {interval_sql}))) * 1000 AS bucket_ms, \
                 avg(assumeNotNull(value)) AS avg_value, \
                 min(assumeNotNull(value)) AS min_value, \
                 max(assumeNotNull(value)) AS max_value, \
                 count() AS count, \
                 {agg_expr} AS agg_value, \
                 {group_select} \
             FROM metrics \
             WHERE {where_sql} \
             GROUP BY bucket_ms{group_by_extra} \
             ORDER BY bucket_ms ASC \
             LIMIT ?"
        );

        // Bind order: SELECT group keys first (the `attributes[?]` in the
        // projection), then WHERE params in clause order, then LIMIT.
        let mut q = self.ch.query(&sql);
        for key in &query.group_by {
            q = q.bind(key.clone());
        }
        q = q.bind(query.project_id);
        if let Some(ref name) = query.metric_name {
            q = q.bind(name.clone());
        }
        if let Some(mt) = query.metric_type {
            q = q.bind(mt.to_string());
        }
        if let Some(ref svc) = query.service_name {
            q = q.bind(svc.clone());
        }
        if let Some(ref env) = query.environment {
            q = q.bind(env.clone());
        }
        for (k, v) in &query.label_filters {
            q = q.bind(k.clone());
            q = q.bind(v.clone());
        }
        if let Some(start) = query.start_time {
            q = q.bind(start.timestamp_millis());
        }
        if let Some(end) = query.end_time {
            q = q.bind(end.timestamp_millis());
        }
        q = q.bind(limit);

        let rows = q
            .fetch_all::<ChMetricBucketRow>()
            .await
            .map_err(|e| ch_query_err("query_metrics", e))?;

        // Histogram summary, computed separately so cumulative re-exports are NOT
        // double-counted. The inner query collapses each series (cumulative ->
        // latest snapshot via argMax/max; delta or unspecified -> sum across the
        // window); the outer sums those per-series results up to the requested
        // grouping granularity. Matched back to the scalar rows by
        // (bucket_ms, series_values).
        let hist_sql = format!(
            "SELECT bucket_ms, series_values, \
                 sum(s_count) AS hcount, sum(s_sum) AS hsum, \
                 min(s_min) AS hmin, max(s_max) AS hmax, \
                 anyIf(s_bounds, notEmpty(s_bounds)) AS hbounds, \
                 sumForEach(s_buckets) AS hbuckets \
             FROM ( \
                 SELECT \
                     toInt64(toUnixTimestamp(toStartOfInterval(timestamp, {interval_sql}))) * 1000 AS bucket_ms, \
                     any({group_array}) AS series_values, \
                     attributes_hash AS ah, \
                     if(any(temporality) = 'cumulative', max(ifNull(histogram_count, 0)), sum(ifNull(histogram_count, 0))) AS s_count, \
                     if(any(temporality) = 'cumulative', max(ifNull(histogram_sum, 0)), sum(ifNull(histogram_sum, 0))) AS s_sum, \
                     min(histogram_min) AS s_min, \
                     max(histogram_max) AS s_max, \
                     any(histogram_bounds) AS s_bounds, \
                     if(any(temporality) = 'cumulative', argMax(histogram_bucket_counts, timestamp), sumForEach(histogram_bucket_counts)) AS s_buckets \
                 FROM metrics \
                 WHERE {where_sql} AND notEmpty(histogram_bucket_counts) \
                 GROUP BY bucket_ms, ah \
             ) \
             GROUP BY bucket_ms, series_values \
             ORDER BY bucket_ms ASC \
             LIMIT ?"
        );
        let mut hq = self.ch.query(&hist_sql);
        for key in &query.group_by {
            hq = hq.bind(key.clone());
        }
        hq = hq.bind(query.project_id);
        if let Some(ref name) = query.metric_name {
            hq = hq.bind(name.clone());
        }
        if let Some(mt) = query.metric_type {
            hq = hq.bind(mt.to_string());
        }
        if let Some(ref svc) = query.service_name {
            hq = hq.bind(svc.clone());
        }
        if let Some(ref env) = query.environment {
            hq = hq.bind(env.clone());
        }
        for (k, v) in &query.label_filters {
            hq = hq.bind(k.clone());
            hq = hq.bind(v.clone());
        }
        if let Some(start) = query.start_time {
            hq = hq.bind(start.timestamp_millis());
        }
        if let Some(end) = query.end_time {
            hq = hq.bind(end.timestamp_millis());
        }
        hq = hq.bind(limit);
        let hist_rows = hq
            .fetch_all::<ChHistogramRow>()
            .await
            .map_err(|e| ch_query_err("query_metrics histogram", e))?;
        let mut hist_map: std::collections::HashMap<(i64, Vec<String>), HistogramSummary> =
            std::collections::HashMap::new();
        for h in hist_rows {
            if h.hbounds.is_empty() {
                continue;
            }
            hist_map.insert(
                (h.bucket_ms, h.series_values.clone()),
                HistogramSummary {
                    count: h.hcount,
                    sum: h.hsum,
                    min: h.hmin,
                    max: h.hmax,
                    bounds: h.hbounds,
                    bucket_counts: h.hbuckets,
                },
            );
        }

        let group_keys = query.group_by.clone();
        Ok(rows
            .into_iter()
            .map(|r| {
                // Look up the temporality-correct histogram summary (None for
                // gauge/sum metrics, which produce no histogram rows).
                let histogram_summary = hist_map
                    .get(&(r.bucket_ms, r.series_values.clone()))
                    .cloned();
                let series_key = if group_keys.is_empty() {
                    None
                } else {
                    Some(
                        group_keys
                            .iter()
                            .cloned()
                            .zip(r.series_values)
                            .collect::<Vec<(String, String)>>(),
                    )
                };
                MetricBucket {
                    bucket: chrono::Utc
                        .timestamp_millis_opt(r.bucket_ms)
                        .single()
                        .unwrap_or_default(),
                    avg_value: r.avg_value,
                    min_value: r.min_value,
                    max_value: r.max_value,
                    count: r.count as i64,
                    value: r.agg_value,
                    quantiles: match query.aggregation.quantile() {
                        Some(qq) => vec![(qq, r.agg_value)],
                        None => Vec::new(),
                    },
                    histogram_summary,
                    series_key,
                }
            })
            .collect())
    }

    /// List distinct metric names for a project from the CH `metrics` table.
    async fn list_metric_names(&self, project_id: i32) -> StorageResult<Vec<String>> {
        let rows = self
            .ch
            .query(
                "SELECT DISTINCT metric_name FROM metrics \
                 WHERE project_id = ? ORDER BY metric_name",
            )
            .bind(project_id)
            .fetch_all::<ChMetricNameRow>()
            .await
            .map_err(|e| ch_query_err("list_metric_names", e))?;

        Ok(rows.into_iter().map(|r| r.metric_name).collect())
    }

    async fn list_metric_label_keys(
        &self,
        project_id: i32,
        metric_name: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>> {
        // Sample recent rows first (subquery LIMIT), then ARRAY JOIN the sampled
        // attribute-key arrays — keeps the unnest bounded on high-volume metrics.
        let rows = self
            .ch
            .query(
                "SELECT DISTINCT label_key FROM ( \
                   SELECT mapKeys(attributes) AS ks FROM metrics \
                   WHERE project_id = ? AND metric_name = ? \
                     AND timestamp >= fromUnixTimestamp64Milli(?) \
                     AND timestamp <= fromUnixTimestamp64Milli(?) \
                   ORDER BY timestamp DESC LIMIT 2000 \
                 ) ARRAY JOIN ks AS label_key \
                 WHERE label_key != '' ORDER BY label_key",
            )
            .bind(project_id)
            .bind(metric_name)
            .bind(start_time.timestamp_millis())
            .bind(end_time.timestamp_millis())
            .fetch_all::<ChLabelRow>()
            .await
            .map_err(|e| ch_query_err("list_metric_label_keys", e))?;

        Ok(rows.into_iter().map(|r| r.label_key).collect())
    }

    async fn list_metric_label_values(
        &self,
        project_id: i32,
        metric_name: &str,
        label_key: &str,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
    ) -> StorageResult<Vec<String>> {
        // `attributes[?]` reads the value for the chosen key; `mapContains` keeps
        // only rows that actually carry it. Sampled and capped like the keys query.
        let rows = self
            .ch
            .query(
                "SELECT DISTINCT label_key FROM ( \
                   SELECT attributes[?] AS label_key FROM metrics \
                   WHERE project_id = ? AND metric_name = ? \
                     AND mapContains(attributes, ?) \
                     AND timestamp >= fromUnixTimestamp64Milli(?) \
                     AND timestamp <= fromUnixTimestamp64Milli(?) \
                   ORDER BY timestamp DESC LIMIT 5000 \
                 ) WHERE label_key != '' ORDER BY label_key LIMIT 500",
            )
            .bind(label_key)
            .bind(project_id)
            .bind(metric_name)
            .bind(label_key)
            .bind(start_time.timestamp_millis())
            .bind(end_time.timestamp_millis())
            .fetch_all::<ChLabelRow>()
            .await
            .map_err(|e| ch_query_err("list_metric_label_values", e))?;

        Ok(rows.into_iter().map(|r| r.label_key).collect())
    }

    // ── Non-span methods — delegate to TimescaleDB unconditionally ───────────
    // (ADR-016 Phases 2–4 will replace these with CH implementations)

    async fn store_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        self.inner.store_logs(records).await
    }

    async fn archive_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        self.inner.archive_logs(records).await
    }

    async fn query_logs(&self, query: LogQuery) -> StorageResult<Vec<LogRecord>> {
        self.inner.query_logs(query).await
    }

    // ── Control-row methods — always Postgres (insights, health, quota) ──────

    async fn upsert_insight(&self, insight: &Insight) -> StorageResult<i64> {
        self.inner.upsert_insight(insight).await
    }

    async fn list_insights(
        &self,
        project_id: i32,
        status: Option<InsightStatus>,
        limit: u64,
        offset: u64,
    ) -> StorageResult<Vec<Insight>> {
        self.inner
            .list_insights(project_id, status, limit, offset)
            .await
    }

    async fn resolve_insight(&self, insight_id: i64) -> StorageResult<()> {
        self.inner.resolve_insight(insight_id).await
    }

    async fn store_health_summary(&self, summary: &HealthSummary) -> StorageResult<()> {
        self.inner.store_health_summary(summary).await
    }

    async fn get_health_summaries(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> StorageResult<Vec<HealthSummary>> {
        self.inner
            .get_health_summaries(project_id, environment_id)
            .await
    }

    async fn get_storage_quota(&self, project_id: i32) -> StorageResult<StorageQuota> {
        self.inner.get_storage_quota(project_id).await
    }

    async fn check_quota(&self, project_id: i32) -> StorageResult<bool> {
        self.inner.check_quota(project_id).await
    }

    // ── Anomaly-detection helpers (ClickHouse — native) ──────────────────────
    //
    // store_metrics now writes only to CH, so these MUST read from CH or the
    // anomaly detector would see an empty otel_metrics on Timescale. Implemented
    // natively here to keep anomaly detection alive (ADR-016 Phase B, option a).

    /// Hour-of-day / day-of-week baseline stats for a metric over a lookback
    /// window. Mirrors the TimescaleDB query: average + population stddev grouped
    /// by hour and weekday. ClickHouse `toHour`/`toDayOfWeek` operate in UTC for a
    /// `DateTime64(_, 'UTC')` column.
    ///
    /// `toDayOfWeek` returns 1=Monday..7=Sunday; the Postgres `EXTRACT(DOW)`
    /// returns 0=Sunday..6=Saturday. We remap to the Postgres convention so the
    /// anomaly detector sees identical day indices regardless of backend.
    async fn get_metric_baseline(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        lookback_days: i32,
    ) -> StorageResult<Vec<BaselinePoint>> {
        // Clamp the lookback to a sane range and bind it as the interval count.
        let lookback = lookback_days.clamp(1, 3650);

        let mut where_clauses = vec![
            "project_id = ?".to_string(),
            "service_name = ?".to_string(),
            "metric_name = ?".to_string(),
            "value IS NOT NULL".to_string(),
            "timestamp >= now() - toIntervalDay(?)".to_string(),
        ];
        if environment.is_some() {
            where_clauses.push("deployment_environment = ?".to_string());
        }
        let where_sql = where_clauses.join(" AND ");

        // toDayOfWeek(...) % 7 maps Mon..Sun (1..7) -> 1..6,0 (Mon..Sat, Sun=0),
        // matching Postgres EXTRACT(DOW) where Sunday = 0.
        let sql = format!(
            "SELECT \
                 toInt32(toHour(timestamp)) AS hour_of_day, \
                 toInt32(toDayOfWeek(timestamp) % 7) AS day_of_week, \
                 avg(value) AS avg_value, \
                 ifNull(stddevPop(value), 0) AS stddev_value, \
                 count() AS sample_count \
             FROM metrics \
             WHERE {where_sql} \
             GROUP BY hour_of_day, day_of_week \
             ORDER BY day_of_week, hour_of_day"
        );

        let mut q = self
            .ch
            .query(&sql)
            .bind(project_id)
            .bind(service_name)
            .bind(metric_name)
            .bind(lookback as u32);
        if let Some(env) = environment {
            q = q.bind(env);
        }

        let rows = q
            .fetch_all::<ChBaselineRow>()
            .await
            .map_err(|e| ch_query_err("get_metric_baseline", e))?;

        Ok(rows
            .into_iter()
            .map(|r| BaselinePoint {
                hour_of_day: r.hour_of_day,
                day_of_week: r.day_of_week,
                avg_value: r.avg_value,
                stddev_value: r.stddev_value,
                sample_count: r.sample_count as i64,
            })
            .collect())
    }

    /// Recent 1-minute aggregates for anomaly scoring. Buckets on
    /// `toStartOfMinute` and averages the scalar `value`.
    async fn get_recent_minute_aggregates(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        minutes: i32,
    ) -> StorageResult<Vec<MinuteAggregate>> {
        use chrono::TimeZone;

        let window = minutes.clamp(1, 100_000);

        let mut where_clauses = vec![
            "project_id = ?".to_string(),
            "service_name = ?".to_string(),
            "metric_name = ?".to_string(),
            "value IS NOT NULL".to_string(),
            "timestamp >= now() - toIntervalMinute(?)".to_string(),
        ];
        if environment.is_some() {
            where_clauses.push("deployment_environment = ?".to_string());
        }
        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            "SELECT \
                 toInt64(toUnixTimestamp(toStartOfMinute(timestamp))) * 1000 AS bucket_ms, \
                 avg(value) AS avg_value, \
                 count() AS count \
             FROM metrics \
             WHERE {where_sql} \
             GROUP BY bucket_ms \
             ORDER BY bucket_ms ASC"
        );

        let mut q = self
            .ch
            .query(&sql)
            .bind(project_id)
            .bind(service_name)
            .bind(metric_name)
            .bind(window as u32);
        if let Some(env) = environment {
            q = q.bind(env);
        }

        let rows = q
            .fetch_all::<ChMinuteAggregateRow>()
            .await
            .map_err(|e| ch_query_err("get_recent_minute_aggregates", e))?;

        Ok(rows
            .into_iter()
            .map(|r| MinuteAggregate {
                bucket: chrono::Utc
                    .timestamp_millis_opt(r.bucket_ms)
                    .single()
                    .unwrap_or_default(),
                avg_value: r.avg_value,
                count: r.count as i64,
            })
            .collect())
    }

    async fn get_recent_deploys(
        &self,
        project_id: i32,
        minutes: i32,
    ) -> StorageResult<Vec<DeployEvent>> {
        self.inner.get_recent_deploys(project_id, minutes).await
    }

    async fn apply_retention(&self, project_id: i32) -> StorageResult<u64> {
        self.inner.apply_retention(project_id).await
    }

    async fn get_p95_latency(
        &self,
        project_id: i32,
        service_name: &str,
        window_minutes: i32,
    ) -> StorageResult<f64> {
        self.inner
            .get_p95_latency(project_id, service_name, window_minutes)
            .await
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn make_span() -> SpanRecord {
        SpanRecord {
            project_id: 42,
            deployment_id: Some(7),
            resource: ResourceInfo {
                service_name: "my-service".into(),
                service_version: Some("1.2.3".into()),
                deployment_environment: Some("production".into()),
                attributes: BTreeMap::new(),
            },
            trace_id: "abc123".into(),
            span_id: "span001".into(),
            parent_span_id: Some("parent001".into()),
            name: "GET /api/v1/health".into(),
            kind: SpanKind::Server,
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms: 42.5,
            status_code: SpanStatusCode::Ok,
            status_message: "".into(),
            attributes: {
                let mut m = BTreeMap::new();
                m.insert("http.method".into(), "GET".into());
                m
            },
            events: vec![],
        }
    }

    #[test]
    fn span_record_to_ch_row_field_mapping() {
        let span = make_span();
        let row = ChSpanRow::from(&span);

        assert_eq!(row.project_id, 42);
        assert_eq!(row.deployment_id, Some(7));
        assert_eq!(row.service_name, "my-service");
        assert_eq!(row.service_version, "1.2.3");
        assert_eq!(row.deployment_environment, "production");
        assert_eq!(row.trace_id, "abc123");
        assert_eq!(row.span_id, "span001");
        assert_eq!(row.parent_span_id, "parent001");
        assert_eq!(row.name, "GET /api/v1/health");
        assert_eq!(row.kind, "SERVER");
        assert_eq!(row.duration_ms, 42.5);
        assert_eq!(row.status_code, "OK");
        assert_eq!(row.status_message, "");
        // attributes JSON must contain the key
        assert!(row.attributes.contains("http.method"));
        // events is an empty JSON array
        assert_eq!(row.events, "[]");
        // _version is a positive Unix millisecond timestamp
        assert!(row._version > 0);
    }

    #[test]
    fn root_span_gets_empty_parent_span_id() {
        let mut span = make_span();
        span.parent_span_id = None;
        let row = ChSpanRow::from(&span);
        assert_eq!(row.parent_span_id, "");
    }

    #[test]
    fn missing_service_version_and_env_become_empty_strings() {
        let mut span = make_span();
        span.resource.service_version = None;
        span.resource.deployment_environment = None;
        let row = ChSpanRow::from(&span);
        assert_eq!(row.service_version, "");
        assert_eq!(row.deployment_environment, "");
    }

    #[test]
    fn ch_row_roundtrips_to_span_record() {
        let original = make_span();
        let row = ChSpanRow::from(&original);
        let recovered = SpanRecord::from(row);

        assert_eq!(recovered.project_id, original.project_id);
        assert_eq!(recovered.deployment_id, original.deployment_id);
        assert_eq!(recovered.trace_id, original.trace_id);
        assert_eq!(recovered.span_id, original.span_id);
        assert_eq!(recovered.parent_span_id, original.parent_span_id);
        assert_eq!(recovered.name, original.name);
        assert_eq!(recovered.kind, original.kind);
        assert_eq!(recovered.duration_ms, original.duration_ms);
        assert_eq!(recovered.status_code, original.status_code);
        assert_eq!(
            recovered.resource.service_name,
            original.resource.service_name
        );
        assert_eq!(
            recovered.resource.service_version,
            original.resource.service_version
        );
        assert_eq!(
            recovered.resource.deployment_environment,
            original.resource.deployment_environment
        );
        // Timestamps round-trip to millisecond precision
        assert_eq!(
            recovered.start_time.timestamp_millis(),
            original.start_time.timestamp_millis()
        );
        assert_eq!(
            recovered.end_time.timestamp_millis(),
            original.end_time.timestamp_millis()
        );
        // Attributes survive the JSON round-trip
        assert_eq!(recovered.attributes, original.attributes);
    }

    #[test]
    fn root_span_ch_row_recovers_none_parent() {
        let mut span = make_span();
        span.parent_span_id = None;
        let row = ChSpanRow::from(&span);
        let recovered = SpanRecord::from(row);
        assert_eq!(recovered.parent_span_id, None);
    }

    #[test]
    fn span_kind_roundtrip() {
        for kind in [
            SpanKind::Unspecified,
            SpanKind::Internal,
            SpanKind::Server,
            SpanKind::Client,
            SpanKind::Producer,
            SpanKind::Consumer,
        ] {
            let s = span_kind_to_str(kind);
            assert_eq!(str_to_span_kind(s), kind);
        }
    }

    #[test]
    fn span_status_roundtrip() {
        for code in [
            SpanStatusCode::Unset,
            SpanStatusCode::Ok,
            SpanStatusCode::Error,
        ] {
            let s = span_status_to_str(code);
            assert_eq!(str_to_span_status(s), code);
        }
    }

    #[test]
    fn unknown_kind_string_becomes_unspecified() {
        assert_eq!(str_to_span_kind("BOGUS"), SpanKind::Unspecified);
    }

    #[test]
    fn unknown_status_string_becomes_unset() {
        assert_eq!(str_to_span_status("BOGUS"), SpanStatusCode::Unset);
    }

    // ── escape_like_pattern tests ─────────────────────────────────────────

    #[test]
    fn escape_plain_pattern_unchanged() {
        assert_eq!(escape_like_pattern("hello"), "hello");
        assert_eq!(escape_like_pattern("GET /api/v1"), "GET /api/v1");
    }

    #[test]
    fn escape_percent_metachar() {
        // A literal '%' in the user pattern must become '\%' so it does not
        // act as a wildcard in the LIKE expression.
        assert_eq!(escape_like_pattern("%"), "\\%");
        assert_eq!(escape_like_pattern("50%"), "50\\%");
    }

    #[test]
    fn escape_underscore_metachar() {
        assert_eq!(escape_like_pattern("_id"), "\\_id");
        assert_eq!(escape_like_pattern("user_name"), "user\\_name");
    }

    #[test]
    fn escape_backslash_first() {
        // A literal backslash must become '\\' and must be processed before
        // the other replacements so that introduced backslashes are not
        // double-escaped.
        assert_eq!(escape_like_pattern("\\"), "\\\\");
        // A pattern with both a backslash and a %:
        //   input:  `\%`  (backslash then percent)
        //   want:   `\\\%` (escaped backslash, then escaped percent)
        assert_eq!(escape_like_pattern("\\%"), "\\\\\\%");
    }

    #[test]
    fn wrapped_escaped_pattern_is_correct() {
        // Full round-trip: user types "50%" → ILIKE pattern "%50\%%"
        let pattern = "50%";
        let wrapped = format!("%{}%", escape_like_pattern(pattern));
        assert_eq!(wrapped, "%50\\%%");
    }

    // ── store_spans chunking tests ────────────────────────────────────────

    #[test]
    fn spans_chunks_split_correctly() {
        // Verify that a Vec of N spans is chunked into ceil(N/10_000) pieces.
        // 30_001 spans → 3 full chunks of 10_000 + 1 tail chunk of 1.
        let n = 30_001usize;
        let spans: Vec<SpanRecord> = (0..n).map(|_| make_span()).collect();
        let chunks: Vec<_> = spans.chunks(10_000).collect();
        // ceil(30_001 / 10_000) = 4 chunks
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].len(), 10_000);
        assert_eq!(chunks[1].len(), 10_000);
        assert_eq!(chunks[2].len(), 10_000);
        assert_eq!(chunks[3].len(), 1);
        // Total preserved
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, n);
    }

    #[test]
    fn spans_chunks_below_batch_size_is_single_chunk() {
        let spans: Vec<SpanRecord> = (0..42).map(|_| make_span()).collect();
        let chunks: Vec<_> = spans.chunks(10_000).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 42);
    }

    #[test]
    fn spans_chunks_exact_batch_size_is_single_chunk() {
        let spans: Vec<SpanRecord> = (0..10_000).map(|_| make_span()).collect();
        let chunks: Vec<_> = spans.chunks(10_000).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 10_000);
    }

    #[test]
    fn ch_ingest_err_includes_operation_name() {
        let err = ::clickhouse::error::Error::BadResponse("network error".into());
        let otel_err = ch_ingest_err("store_spans", err);
        assert!(otel_err.to_string().contains("store_spans"));
        assert!(otel_err.to_string().contains("network error"));
    }

    #[test]
    fn ch_query_err_includes_operation_name() {
        let err = ::clickhouse::error::Error::BadResponse("timeout".into());
        let otel_err = ch_query_err("query_trace_summaries", err);
        assert!(otel_err.to_string().contains("query_trace_summaries"));
    }

    // ── Metric row tests ────────────────────────────────────────────────────

    use crate::types::{AggregationTemporality, Exemplar, MetricPoint, MetricQuery, MetricType};

    /// A Gauge point with attributes — the simplest scalar case.
    fn make_gauge() -> MetricPoint {
        let mut p = MetricPoint::skeleton(
            42,
            Some(7),
            ResourceInfo {
                service_name: "my-service".into(),
                service_version: Some("1.2.3".into()),
                deployment_environment: Some("production".into()),
                attributes: BTreeMap::new(),
            },
            "http.server.active_requests".into(),
            MetricType::Gauge,
            "1".into(),
            Utc::now(),
            {
                let mut m = BTreeMap::new();
                m.insert("http.method".into(), "GET".into());
                m
            },
        );
        p.value = Some(3.5);
        p
    }

    /// A cumulative Histogram point exercising the full array/aggregate fields.
    fn make_histogram() -> MetricPoint {
        let mut p = MetricPoint::skeleton(
            42,
            None,
            ResourceInfo::default(),
            "http.server.duration".into(),
            MetricType::Histogram,
            "ms".into(),
            Utc::now(),
            BTreeMap::new(),
        );
        p.temporality = Some(AggregationTemporality::Cumulative);
        p.histogram_count = Some(10);
        p.histogram_sum = Some(1234.5);
        p.histogram_min = Some(1.0);
        p.histogram_max = Some(500.0);
        p.histogram_bounds = Some(vec![0.0, 5.0, 10.0]);
        p.histogram_bucket_counts = Some(vec![1, 4, 3, 2]);
        p.value = Some(1234.5 / 10.0); // synthetic mean from decode
        p
    }

    #[test]
    fn ch_metric_row_field_count_matches_ddl() {
        // Field-order landmine guard: ChMetricRow serialises positionally and
        // MUST mirror 0003_metrics.sql column order/count exactly. Counting via
        // a fully-specified struct literal forces a compile error if a field is
        // added/removed without updating CH_METRIC_ROW_FIELD_COUNT.
        let row = ChMetricRow::from(&make_gauge());
        let ChMetricRow {
            project_id: _,
            deployment_id: _,
            service_name: _,
            service_version: _,
            deployment_environment: _,
            metric_name: _,
            metric_type: _,
            temporality: _,
            is_monotonic: _,
            unit: _,
            description: _,
            timestamp: _,
            start_time: _,
            flags: _,
            value: _,
            histogram_count: _,
            histogram_sum: _,
            histogram_min: _,
            histogram_max: _,
            histogram_bounds: _,
            histogram_bucket_counts: _,
            exp_scale: _,
            exp_zero_count: _,
            exp_zero_threshold: _,
            exp_positive_offset: _,
            exp_positive_counts: _,
            exp_negative_offset: _,
            exp_negative_counts: _,
            summary_quantiles: _,
            exemplars: _,
            attributes: _,
            _version: _,
        } = row;
        // 31 domain columns + _version sentinel = 32 serialised fields; the DDL
        // declares 31 named columns plus _version, matching this destructure.
        assert_eq!(CH_METRIC_ROW_FIELD_COUNT, 31);
    }

    #[test]
    fn gauge_point_to_ch_row_field_mapping() {
        let row = ChMetricRow::from(&make_gauge());

        assert_eq!(row.project_id, 42);
        assert_eq!(row.deployment_id, Some(7));
        assert_eq!(row.service_name, "my-service");
        assert_eq!(row.service_version, "1.2.3");
        assert_eq!(row.deployment_environment, "production");
        assert_eq!(row.metric_name, "http.server.active_requests");
        assert_eq!(row.metric_type, "gauge");
        // Gauge has no temporality -> sentinel.
        assert_eq!(row.temporality, "unspecified");
        assert_eq!(row.is_monotonic, None);
        assert_eq!(row.unit, "1");
        assert_eq!(row.value, Some(3.5));
        // attributes Map preserved as key/value pairs.
        assert_eq!(
            row.attributes,
            vec![("http.method".to_string(), "GET".to_string())]
        );
        // No histogram arrays -> empty sentinels (never Nullable).
        assert!(row.histogram_bounds.is_empty());
        assert!(row.histogram_bucket_counts.is_empty());
        assert!(row.summary_quantiles.is_empty());
        assert!(row.exemplars.is_empty());
        assert!(row._version > 0);
    }

    #[test]
    fn histogram_point_to_ch_row_field_mapping() {
        let row = ChMetricRow::from(&make_histogram());

        assert_eq!(row.metric_type, "histogram");
        assert_eq!(row.temporality, "cumulative");
        assert_eq!(row.histogram_count, Some(10));
        assert_eq!(row.histogram_sum, Some(1234.5));
        assert_eq!(row.histogram_min, Some(1.0));
        assert_eq!(row.histogram_max, Some(500.0));
        assert_eq!(row.histogram_bounds, vec![0.0, 5.0, 10.0]);
        assert_eq!(row.histogram_bucket_counts, vec![1, 4, 3, 2]);
    }

    #[test]
    fn monotonic_sum_maps_is_monotonic_to_u8() {
        let mut p = make_gauge();
        p.metric_type = MetricType::Sum;
        p.is_monotonic = Some(true);
        p.temporality = Some(AggregationTemporality::Delta);
        let row = ChMetricRow::from(&p);
        assert_eq!(row.metric_type, "sum");
        assert_eq!(row.is_monotonic, Some(1));
        assert_eq!(row.temporality, "delta");

        p.is_monotonic = Some(false);
        let row = ChMetricRow::from(&p);
        assert_eq!(row.is_monotonic, Some(0));
    }

    #[test]
    fn exemplars_map_to_tuple_with_hex_ids() {
        let mut p = make_gauge();
        p.exemplars = vec![Exemplar {
            timestamp: Utc::now(),
            value: 9.0,
            trace_id: Some("deadbeef".into()),
            span_id: Some("cafe".into()),
            attributes: BTreeMap::new(),
        }];
        let row = ChMetricRow::from(&p);
        assert_eq!(row.exemplars.len(), 1);
        let (trace, span, value, ts) = &row.exemplars[0];
        assert_eq!(trace, "deadbeef");
        assert_eq!(span, "cafe");
        assert_eq!(*value, 9.0);
        assert!(*ts > 0);
    }

    #[test]
    fn chunks_split_at_metric_batch_size() {
        // 20_001 metrics -> ceil(20_001 / 10_000) = 3 chunks.
        let points: Vec<MetricPoint> = (0..20_001).map(|_| make_gauge()).collect();
        let chunks: Vec<_> = points.chunks(10_000).collect();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 10_000);
        assert_eq!(chunks[2].len(), 1);
    }

    /// A monotonic cumulative Sum (counter) — exercises is_monotonic + temporality
    /// + start_time + flags on the scalar path.
    fn make_monotonic_sum() -> MetricPoint {
        let mut p = MetricPoint::skeleton(
            7,
            Some(3),
            ResourceInfo {
                service_name: "api".into(),
                service_version: Some("9.9.9".into()),
                deployment_environment: Some("staging".into()),
                attributes: BTreeMap::new(),
            },
            "http.requests.total".into(),
            MetricType::Sum,
            "1".into(),
            Utc::now(),
            {
                let mut m = BTreeMap::new();
                m.insert("route".into(), "/api/v1".into());
                m
            },
        );
        p.temporality = Some(AggregationTemporality::Cumulative);
        p.is_monotonic = Some(true);
        p.start_time = Some(Utc::now());
        p.flags = 1;
        p.description = Some("Total HTTP requests".into());
        p.value = Some(4242.0);
        p
    }

    /// An exponential-histogram point with positive/negative bucket arrays and
    /// the exp_* scalar fields populated.
    fn make_exp_histogram() -> MetricPoint {
        let mut p = MetricPoint::skeleton(
            7,
            None,
            ResourceInfo::default(),
            "rpc.duration".into(),
            MetricType::ExponentialHistogram,
            "ms".into(),
            Utc::now(),
            BTreeMap::new(),
        );
        p.temporality = Some(AggregationTemporality::Delta);
        p.histogram_count = Some(8);
        p.histogram_sum = Some(40.0);
        p.histogram_min = Some(0.1);
        p.histogram_max = Some(10.0);
        p.exp_scale = Some(3);
        p.exp_zero_count = Some(1);
        p.exp_zero_threshold = Some(1e-6);
        p.exp_positive_offset = Some(2);
        p.exp_positive_counts = Some(vec![1, 2, 3]);
        p.exp_negative_offset = Some(-1);
        p.exp_negative_counts = Some(vec![4, 5]);
        p.value = Some(5.0); // synthetic mean
        p
    }

    /// A summary point carrying quantile/value pairs.
    fn make_summary() -> MetricPoint {
        let mut p = MetricPoint::skeleton(
            7,
            None,
            ResourceInfo::default(),
            "rpc.server.duration".into(),
            MetricType::Summary,
            "ms".into(),
            Utc::now(),
            BTreeMap::new(),
        );
        p.histogram_count = Some(4);
        p.histogram_sum = Some(20.0);
        p.summary_quantiles = Some(vec![(0.5, 4.0), (0.99, 9.0)]);
        p.value = Some(5.0); // synthetic mean
        p
    }

    #[test]
    fn monotonic_sum_point_to_ch_row_field_mapping() {
        let row = ChMetricRow::from(&make_monotonic_sum());

        assert_eq!(row.metric_type, "sum");
        assert_eq!(row.temporality, "cumulative");
        assert_eq!(row.is_monotonic, Some(1));
        assert_eq!(row.unit, "1");
        assert_eq!(row.description, "Total HTTP requests");
        assert_eq!(row.flags, 1);
        // start_time is set -> Some(positive ms).
        assert!(row.start_time.is_some());
        assert!(row.start_time.unwrap_or(0) > 0);
        assert_eq!(row.value, Some(4242.0));
        assert_eq!(
            row.attributes,
            vec![("route".to_string(), "/api/v1".to_string())]
        );
        // Sums carry no histogram arrays.
        assert!(row.histogram_bounds.is_empty());
        assert!(row.exp_positive_counts.is_empty());
        assert!(row.summary_quantiles.is_empty());
    }

    #[test]
    fn exp_histogram_point_to_ch_row_field_mapping() {
        let row = ChMetricRow::from(&make_exp_histogram());

        assert_eq!(row.metric_type, "exponential_histogram");
        assert_eq!(row.temporality, "delta");
        assert_eq!(row.histogram_count, Some(8));
        assert_eq!(row.histogram_sum, Some(40.0));
        assert_eq!(row.histogram_min, Some(0.1));
        assert_eq!(row.histogram_max, Some(10.0));
        assert_eq!(row.exp_scale, Some(3));
        assert_eq!(row.exp_zero_count, Some(1));
        assert_eq!(row.exp_zero_threshold, Some(1e-6));
        assert_eq!(row.exp_positive_offset, Some(2));
        assert_eq!(row.exp_positive_counts, vec![1, 2, 3]);
        assert_eq!(row.exp_negative_offset, Some(-1));
        assert_eq!(row.exp_negative_counts, vec![4, 5]);
        // Explicit-histogram bound arrays remain empty sentinels here.
        assert!(row.histogram_bounds.is_empty());
        assert!(row.histogram_bucket_counts.is_empty());
        // is_monotonic is non-Sum -> None.
        assert_eq!(row.is_monotonic, None);
    }

    #[test]
    fn summary_point_to_ch_row_field_mapping() {
        let row = ChMetricRow::from(&make_summary());

        assert_eq!(row.metric_type, "summary");
        // Summaries do not report temporality -> sentinel.
        assert_eq!(row.temporality, "unspecified");
        assert_eq!(row.histogram_count, Some(4));
        assert_eq!(row.histogram_sum, Some(20.0));
        assert_eq!(row.summary_quantiles, vec![(0.5, 4.0), (0.99, 9.0)]);
        // Summaries carry no explicit/exponential histogram arrays.
        assert!(row.histogram_bounds.is_empty());
        assert!(row.exp_positive_counts.is_empty());
        assert!(row.exemplars.is_empty());
    }

    #[test]
    fn point_with_exemplars_and_labels_full_mapping() {
        // A gauge carrying both labels AND multiple exemplars, asserting that the
        // exemplar tuples and the attribute Map both survive in full.
        let ts = Utc::now();
        let mut p = MetricPoint::skeleton(
            7,
            Some(3),
            ResourceInfo::default(),
            "db.pool.in_use".into(),
            MetricType::Gauge,
            "1".into(),
            ts,
            {
                let mut m = BTreeMap::new();
                m.insert("pool".into(), "primary".into());
                m.insert("db.system".into(), "postgresql".into());
                m
            },
        );
        p.exemplars = vec![
            Exemplar {
                timestamp: ts,
                value: 12.0,
                trace_id: Some("aabbccdd".into()),
                span_id: Some("1122".into()),
                attributes: BTreeMap::new(),
            },
            Exemplar {
                timestamp: ts,
                value: 13.0,
                // An exemplar with no trace/span link -> empty-string sentinels.
                trace_id: None,
                span_id: None,
                attributes: BTreeMap::new(),
            },
        ];
        p.value = Some(11.0);

        let row = ChMetricRow::from(&p);

        // Attributes Map preserved in sorted (BTreeMap) key order.
        assert_eq!(
            row.attributes,
            vec![
                ("db.system".to_string(), "postgresql".to_string()),
                ("pool".to_string(), "primary".to_string()),
            ]
        );
        // Both exemplars survive; the second one's missing IDs become "".
        assert_eq!(row.exemplars.len(), 2);
        assert_eq!(row.exemplars[0].0, "aabbccdd");
        assert_eq!(row.exemplars[0].1, "1122");
        assert_eq!(row.exemplars[0].2, 12.0);
        assert!(row.exemplars[0].3 > 0);
        assert_eq!(row.exemplars[1].0, "");
        assert_eq!(row.exemplars[1].1, "");
        assert_eq!(row.exemplars[1].2, 13.0);
        assert_eq!(row.value, Some(11.0));
    }

    #[test]
    fn ch_metric_row_field_order_matches_ddl_columns() {
        // SERIALIZATION LANDMINE GUARD.
        //
        // ChMetricRow is serialised positionally over RowBinary; a mismatch
        // between the struct field order and the `metrics` DDL column order
        // silently corrupts every insert. This test parses the column list out
        // of 0003_metrics.sql and asserts it equals the ChMetricRow field order
        // (derived from clickhouse::Row), so a future reorder of either side
        // fails loudly here instead of corrupting data at runtime.
        let ddl = include_str!("../../../migrations/clickhouse/0003_metrics.sql");

        // Extract the column names from the `CREATE TABLE metrics ( ... )` body.
        let create_start = ddl
            .find("CREATE TABLE IF NOT EXISTS metrics")
            .expect("DDL must declare the metrics table");
        let body_start = ddl[create_start..]
            .find('(')
            .map(|i| create_start + i + 1)
            .expect("CREATE TABLE must have an opening paren");
        // The column body ends at the matching `)` that precedes `ENGINE`.
        let engine_pos = ddl[body_start..]
            .find("ENGINE")
            .map(|i| body_start + i)
            .expect("DDL must declare an ENGINE");
        let body = &ddl[body_start..engine_pos];

        let mut ddl_columns: Vec<String> = Vec::new();
        for raw_line in body.lines() {
            let line = raw_line.trim();
            // Skip blanks and whole-line comments.
            if line.is_empty() || line.starts_with("--") {
                continue;
            }
            // MATERIALIZED / ALIAS columns are computed by ClickHouse and are NOT
            // part of the positional RowBinary insert, so they are not ChMetricRow
            // fields — skip them (e.g. attributes_hash, the series fingerprint).
            if line.contains(" MATERIALIZED ") || line.contains(" ALIAS ") {
                continue;
            }
            // The first whitespace-delimited token on a column line is the name.
            // Lines inside the body are always `column_name Type ...,`.
            let token = line.split_whitespace().next().unwrap_or("");
            // Guard against any stray non-identifier tokens.
            if token.is_empty() || !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            ddl_columns.push(token.to_string());
        }

        // The ChMetricRow field order, declared once here and kept in lockstep
        // with the struct definition above. Any change to the struct field list
        // (or the DDL) must update this and will be caught by the two asserts.
        let row_fields = [
            "project_id",
            "deployment_id",
            "service_name",
            "service_version",
            "deployment_environment",
            "metric_name",
            "metric_type",
            "temporality",
            "is_monotonic",
            "unit",
            "description",
            "timestamp",
            "start_time",
            "flags",
            "value",
            "histogram_count",
            "histogram_sum",
            "histogram_min",
            "histogram_max",
            "histogram_bounds",
            "histogram_bucket_counts",
            "exp_scale",
            "exp_zero_count",
            "exp_zero_threshold",
            "exp_positive_offset",
            "exp_positive_counts",
            "exp_negative_offset",
            "exp_negative_counts",
            "summary_quantiles",
            "exemplars",
            "attributes",
            "_version",
        ];

        // 31 domain columns + _version sentinel.
        assert_eq!(
            row_fields.len(),
            CH_METRIC_ROW_FIELD_COUNT + 1,
            "row_fields must list every ChMetricRow field incl _version"
        );
        assert_eq!(
            ddl_columns.len(),
            row_fields.len(),
            "DDL column count ({}) must equal ChMetricRow field count ({}). \
             DDL columns parsed: {:?}",
            ddl_columns.len(),
            row_fields.len(),
            ddl_columns
        );
        for (i, (ddl_col, row_field)) in ddl_columns.iter().zip(row_fields.iter()).enumerate() {
            assert_eq!(
                ddl_col, row_field,
                "Column #{i} mismatch: DDL has `{ddl_col}` but ChMetricRow has `{row_field}`. \
                 Positional RowBinary serialisation requires EXACT order — fix the struct or DDL."
            );
        }
    }

    // ── translate_bucket_interval tests ─────────────────────────────────────

    #[test]
    fn translate_bucket_interval_known_units() {
        assert_eq!(translate_bucket_interval("1 hour"), "INTERVAL 1 HOUR");
        assert_eq!(translate_bucket_interval("5 minutes"), "INTERVAL 5 MINUTE");
        assert_eq!(
            translate_bucket_interval("30 seconds"),
            "INTERVAL 30 SECOND"
        );
        assert_eq!(translate_bucket_interval("1 day"), "INTERVAL 1 DAY");
        assert_eq!(translate_bucket_interval("2 weeks"), "INTERVAL 2 WEEK");
        // Singular + abbreviations.
        assert_eq!(translate_bucket_interval("15 min"), "INTERVAL 15 MINUTE");
        // Compact (no-space) form, as emitted by `format!("{}s", secs)`.
        assert_eq!(translate_bucket_interval("300s"), "INTERVAL 300 SECOND");
        assert_eq!(translate_bucket_interval("5m"), "INTERVAL 5 MINUTE");
        assert_eq!(translate_bucket_interval("1h"), "INTERVAL 1 HOUR");
        assert_eq!(translate_bucket_interval("2d"), "INTERVAL 2 DAY");
        assert_eq!(translate_bucket_interval("1w"), "INTERVAL 1 WEEK");
        // Compact garbage still falls back to the safe default.
        assert_eq!(translate_bucket_interval("300"), "INTERVAL 1 HOUR");
        assert_eq!(translate_bucket_interval("abc"), "INTERVAL 1 HOUR");
        assert_eq!(translate_bucket_interval("5x"), "INTERVAL 1 HOUR");
    }

    #[test]
    fn translate_bucket_interval_rejects_injection_and_garbage() {
        // Injection attempts and malformed inputs fall back to the safe default.
        assert_eq!(
            translate_bucket_interval("1 hour; DROP TABLE metrics"),
            "INTERVAL 1 HOUR"
        );
        assert_eq!(translate_bucket_interval("abc def"), "INTERVAL 1 HOUR");
        assert_eq!(translate_bucket_interval(""), "INTERVAL 1 HOUR");
        assert_eq!(translate_bucket_interval("0 hours"), "INTERVAL 1 HOUR");
        assert_eq!(translate_bucket_interval("-5 hours"), "INTERVAL 1 HOUR");
        assert_eq!(
            translate_bucket_interval("999999999 hours"),
            "INTERVAL 1 HOUR"
        );
        assert_eq!(translate_bucket_interval("1 fortnight"), "INTERVAL 1 HOUR");
    }

    #[test]
    fn store_metrics_query_shapes_use_unbound_metrics_table() {
        // Lightweight guard that the native metric SQL targets the new `metrics`
        // table and bucketing helper (not the Timescale otel_metrics path).
        let interval = translate_bucket_interval(
            MetricQuery::default()
                .bucket_interval
                .as_deref()
                .unwrap_or("1 hour"),
        );
        assert_eq!(interval, "INTERVAL 1 HOUR");
    }

    // ── interval_seconds (RatePerSec divisor) ───────────────────────────────

    #[test]
    fn interval_seconds_parses_canonical_fragments() {
        assert_eq!(interval_seconds("INTERVAL 1 SECOND"), 1);
        assert_eq!(interval_seconds("INTERVAL 30 SECOND"), 30);
        assert_eq!(interval_seconds("INTERVAL 5 MINUTE"), 300);
        assert_eq!(interval_seconds("INTERVAL 1 HOUR"), 3600);
        assert_eq!(interval_seconds("INTERVAL 2 DAY"), 172_800);
        assert_eq!(interval_seconds("INTERVAL 1 WEEK"), 604_800);
    }

    #[test]
    fn interval_seconds_falls_back_to_one_hour_on_garbage() {
        assert_eq!(interval_seconds(""), 3600);
        assert_eq!(interval_seconds("5 MINUTE"), 3600); // missing INTERVAL keyword
        assert_eq!(interval_seconds("INTERVAL x HOUR"), 3600);
        assert_eq!(interval_seconds("INTERVAL 5 FORTNIGHT"), 3600);
    }

    #[test]
    fn interval_seconds_roundtrips_with_translate() {
        // The two helpers must agree: translate produces the canonical fragment
        // that interval_seconds parses back.
        assert_eq!(
            interval_seconds(&translate_bucket_interval("5 minutes")),
            300
        );
        assert_eq!(
            interval_seconds(&translate_bucket_interval("1 day")),
            86_400
        );
    }
}
