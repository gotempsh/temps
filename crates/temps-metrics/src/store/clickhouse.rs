// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ClickHouse-backed implementation of [`MetricsStore`] for resource metrics.
//!
//! Active only when the monitoring store is set to ClickHouse AND all four
//! `TEMPS_CLICKHOUSE_*` env vars are populated. When disabled, the
//! [`TimescaleMetricsStore`](crate::store::timescale::TimescaleMetricsStore)
//! path is used unchanged.
//!
//! # Design (locked)
//!
//! One raw table (`service_metrics`) with a native TTL for retention and
//! **query-time** rollup — no hourly/daily AggregatingMergeTree MVs. Time
//! bucketing is done with `toStartOfInterval()` at read time. See
//! `migrations/clickhouse/0001_service_metrics.sql` for the full schema
//! rationale and benchmark.
//!
//! # Result parity
//!
//! Every method returns the exact same shape as `TimescaleMetricsStore`:
//! - `write_batch` → `()`
//! - `query_range` → `Vec<(DateTime<Utc>, f64)>` (bucket, avg/delta)
//! - `query_latest` → `HashMap<String, f64>`
//! - `query_latest_by_label` → `Vec<LabelledMetric>`
//! - `latest_timestamp` → `Option<DateTime<Utc>>`
//! - `prune` → `u64`
//!
//! # Security
//!
//! - Metric names are validated against the `[a-zA-Z0-9_.:-]` allowlist
//!   (shared [`validate_metric_name`]) at the SAME points as the TimescaleDB
//!   store, BEFORE any name reaches SQL.
//! - The configured database name is validated with `[A-Za-z0-9_]` before DDL
//!   (in `clickhouse_migrations`).
//! - All values (source_id, time bounds, step, label key/value) are passed via
//!   bind params (`?` placeholders), never string-interpolated. Validated
//!   metric names are the only interpolated user-controlled tokens, and they
//!   pass the allowlist first.

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::warn;

use crate::error::MetricsError;
use crate::store::timescale::validate_metric_name;
use crate::store::{
    LabelledMetric, LatestByLabelQuery, LatestQuery, MetricKind, MetricPoint, MetricsStore,
    RangeQuery, SourceKind,
};

/// Maximum rows per ClickHouse HTTP insert request. Bounds peak buffer memory
/// in the `clickhouse` client on large scrape batches. The total stored count
/// is always the full (valid) input length.
const MAX_INSERT_BATCH: usize = 10_000;

// ── Client configuration ────────────────────────────────────────────────────

/// Connection configuration for the ClickHouse resource-metrics backend.
///
/// Built from `ServerConfig` fields populated by the `TEMPS_CLICKHOUSE_*`
/// environment variables. All four fields are required.
#[derive(Clone)]
pub struct ClickHouseMetricsConfig {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
    /// Wire compression, resolved from the stored `clickhouse.compression`
    /// setting. [`ClickHouseMetricsConfig::new`] starts at the default (LZ4).
    pub compression: temps_clickhouse::ClickHouseCompression,
}

// Manual Debug that masks the password so it can never leak into logs.
impl std::fmt::Debug for ClickHouseMetricsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseMetricsConfig")
            .field("url", &self.url)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"***")
            .field("compression", &self.compression)
            .finish()
    }
}

impl ClickHouseMetricsConfig {
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
            compression: temps_clickhouse::ClickHouseCompression::default(),
        }
    }

    /// Use `compression` for the client built from this config.
    pub fn with_compression(
        mut self,
        compression: temps_clickhouse::ClickHouseCompression,
    ) -> Self {
        self.compression = compression;
        self
    }
}

// ── Row type ────────────────────────────────────────────────────────────────

/// ClickHouse row matching the `service_metrics` table DDL in
/// `0001_service_metrics.sql`.
///
/// **Field order MUST match the DDL column order exactly.** The `clickhouse`
/// crate serialises fields positionally (binary protocol over HTTP); any
/// reordering relative to the DDL silently corrupts inserts.
///
/// ## Type mapping
///
/// | DDL type                  | Rust type      | Notes                              |
/// |---------------------------|----------------|------------------------------------|
/// | `DateTime64(3, 'UTC')`    | `i64`          | Unix milliseconds                  |
/// | `LowCardinality(String)`  | `String`       | source_kind / name / kind / engine |
/// | `Int32`                   | `i32`          | source_id                          |
/// | `Float64`                 | `f64`          | value                              |
/// | `Nullable(Int32)`         | `Option<i32>`  | node_id                            |
/// | `String`                  | `String`       | labels (serde_json)                |
/// | `UInt64`                  | `u64`          | _version                           |
///
/// `engine` / `environment` map from Rust `Option<String>` to `""` for `None`
/// (the canonical "unset" sentinel; LowCardinality cannot be cheaply nullable).
#[derive(::clickhouse::Row, Serialize, Deserialize, Debug, Clone)]
pub struct ChMetricRow {
    /// time  DateTime64(3, 'UTC') — stored as Unix milliseconds.
    pub time: i64,
    /// source_kind  LowCardinality(String)
    pub source_kind: String,
    /// source_id  Int32
    pub source_id: i32,
    /// name  LowCardinality(String)
    pub name: String,
    /// value  Float64
    pub value: f64,
    /// kind  LowCardinality(String) — "gauge" | "counter"
    pub kind: String,
    /// engine  LowCardinality(String) DEFAULT '' — '' == unset
    pub engine: String,
    /// environment  LowCardinality(String) DEFAULT '' — '' == unset
    pub environment: String,
    /// node_id  Nullable(Int32)
    pub node_id: Option<i32>,
    /// labels  String DEFAULT '{}' — serde_json of HashMap<String,String>
    pub labels: String,
    /// _version  UInt64 — Unix-ms dedup key for ReplacingMergeTree.
    pub _version: u64,
}

/// Map a [`MetricKind`] to the string stored in the CH `kind` column.
fn metric_kind_to_str(kind: &MetricKind) -> &'static str {
    match kind {
        MetricKind::Gauge => "gauge",
        MetricKind::Counter => "counter",
    }
}

impl ChMetricRow {
    /// Convert a [`MetricPoint`] into a row, returning the serialized labels
    /// error path explicitly so the caller can surface
    /// [`MetricsError::SerializationError`] (matching the TimescaleDB store).
    ///
    /// Not a `From` impl because label serialization can fail and `From`
    /// cannot return a `Result`.
    fn try_from_point(p: &MetricPoint) -> Result<Self, MetricsError> {
        // Serialize labels DETERMINISTICALLY. `MetricPoint.labels` is a
        // `HashMap`, whose iteration order Rust randomizes per process — so the
        // same logical label set would serialize to different JSON byte strings
        // across runs. `labels` is part of the ClickHouse ORDER BY (the
        // ReplacingMergeTree dedup key), so two scrapes that re-serialize
        // identical labels in different key order would land as TWO distinct
        // series that never dedup. Collecting into a `BTreeMap` first sorts the
        // keys, so identical labels always produce identical bytes and the
        // dedup contract holds. (PostgreSQL's `jsonb` is already canonical, so
        // this also aligns the live path with the backfill path.)
        let sorted: std::collections::BTreeMap<&String, &String> = p.labels.iter().collect();
        let labels =
            serde_json::to_string(&sorted).map_err(|_| MetricsError::SerializationError)?;

        // _version: Unix-ms at conversion time (ingest moment). Retried scrape
        // batches produce a higher _version and win the ReplacingMergeTree
        // dedup — the CH analog of `ON CONFLICT DO NOTHING`.
        let version = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Ok(Self {
            time: p.time.timestamp_millis(),
            source_kind: p.source_kind.as_str().to_owned(),
            source_id: p.source_id,
            name: p.name.clone(),
            value: p.value,
            kind: metric_kind_to_str(&p.kind).to_owned(),
            engine: p.engine.clone().unwrap_or_default(),
            environment: p.environment.clone().unwrap_or_default(),
            node_id: p.node_id,
            labels,
            _version: version,
        })
    }
}

// ── Read-side row types ─────────────────────────────────────────────────────

/// One bucketed point: `(bucket_ms, value)`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChRangeRow {
    bucket_ms: i64,
    avg_value: f64,
}

/// One `(name, value)` row for `query_latest`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChLatestRow {
    name: String,
    value: f64,
}

/// One `(name, label_value, value)` row for `query_latest_by_label`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChLabelledRow {
    name: String,
    label_value: String,
    value: f64,
}

/// Single nullable timestamp for `latest_timestamp` (0 when no rows).
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChMaxTimeRow {
    /// `max(time)` as Unix-ms; 0 indicates the empty-set sentinel.
    max_ms: i64,
}

/// Convert a Unix-ms value back to `DateTime<Utc>`.
fn ms_to_dt(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

/// How far back from a source's newest sample the latest-value reads look.
/// Same window as the TimescaleDB store's `LATEST_WINDOW`.
const LATEST_WINDOW: &str = "INTERVAL 15 MINUTE";

/// Lookbacks tried, narrowest first, to find a source's newest sample.
///
/// The sort key is `(source_kind, source_id, name, time, labels)`, so `time`
/// is not ordered within a source and an unbounded `max(time)` reads the
/// `time` column of every row the source has stored, up to the 90-day TTL
/// (issue #1616: ~23M rows per node poll). A constant lower bound lets
/// ClickHouse skip old parts by their `time` min/max and old granules by the
/// primary key, so a live source is answered from the first tier. Only a
/// source silent for over 7 days reaches the unbounded tier.
const NEWEST_SAMPLE_LOOKBACKS: [Option<&str>; 4] = [
    Some("INTERVAL 15 MINUTE"),
    Some("INTERVAL 1 DAY"),
    Some("INTERVAL 7 DAY"),
    None,
];

/// `max(time)` of a source, restricted to `lookback` before now when given.
/// Binds: `source_kind`, `source_id`.
fn newest_sample_sql(lookback: Option<&str>) -> String {
    let bound = lookback
        .map(|interval| format!(" AND time > now64(3) - {interval}"))
        .unwrap_or_default();
    format!(
        "SELECT toUnixTimestamp64Milli(max(time)) AS max_ms \
         FROM service_metrics \
         WHERE source_kind = ? AND source_id = ?{bound}"
    )
}

/// Lower time bound for the latest-value reads: [`LATEST_WINDOW`] before the
/// source's newest sample, passed as a literal so ClickHouse prunes on it.
/// Binds: the newest sample's Unix-ms (from [`newest_sample_sql`]).
fn latest_window_bound() -> String {
    format!("time > fromUnixTimestamp64Milli(?) - {LATEST_WINDOW}")
}

/// Most-recent value per metric name. `name_filter` is empty or
/// ` AND name IN (...)` of validated names.
/// Binds: `source_kind`, `source_id`, newest sample ms.
fn latest_sql(name_filter: &str) -> String {
    format!(
        "SELECT name, value \
         FROM service_metrics FINAL \
         WHERE source_kind = ? AND source_id = ?{name_filter} \
           AND {bound} \
         ORDER BY name, length(JSONExtractKeys(labels)) ASC, time DESC \
         LIMIT 1 BY name",
        bound = latest_window_bound(),
    )
}

/// Most-recent value per `(name, label_value)`. `list` is a quoted list of
/// validated names.
/// Binds: label key, `source_kind`, `source_id`, newest sample ms, label key.
fn latest_by_label_sql(list: &str) -> String {
    format!(
        "SELECT name, JSONExtractString(labels, ?) AS label_value, value \
         FROM service_metrics FINAL \
         WHERE source_kind = ? AND source_id = ? \
           AND {bound} \
           AND name IN ({list}) \
           AND JSONHas(labels, ?) \
         ORDER BY name, label_value, time DESC \
         LIMIT 1 BY name, label_value",
        bound = latest_window_bound(),
    )
}

/// Fewest label keys across a metric's most-recent 64 rows. `name` must be
/// validated.
/// Binds: `source_kind`, `source_id`, newest sample ms.
fn min_label_key_count_sql(name: &str) -> String {
    format!(
        "SELECT min(k) AS min_keys FROM ( \
           SELECT toInt64(length(JSONExtractKeys(labels))) AS k \
           FROM service_metrics \
           WHERE source_kind = ? AND source_id = ? AND name = '{name}' \
             AND {bound} \
           ORDER BY time DESC LIMIT 64 \
         )",
        bound = latest_window_bound(),
    )
}

// ── Store ───────────────────────────────────────────────────────────────────

/// ClickHouse-backed [`MetricsStore`].
///
/// The client is cheap to clone (Arc-backed internally). Construction does no
/// I/O; run migrations separately via
/// [`crate::store::clickhouse_migrations::apply_migrations`].
pub struct ClickhouseMetricsStore {
    client: ::clickhouse::Client,
}

impl ClickhouseMetricsStore {
    /// Build a store from connection configuration. Does not validate
    /// connectivity.
    pub fn new(config: ClickHouseMetricsConfig) -> Self {
        let client = temps_clickhouse::build_client(
            &config.url,
            &config.database,
            &config.user,
            &config.password,
            config.compression,
        );
        Self { client }
    }

    /// Borrow the underlying client (for migrations / health checks).
    pub fn client(&self) -> &::clickhouse::Client {
        &self.client
    }

    /// Verify connectivity and authentication with `SELECT 1`.
    pub async fn health_check(&self) -> Result<(), MetricsError> {
        self.client
            .query("SELECT 1")
            .fetch_one::<u8>()
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "health_check".to_string(),
                reason: e.to_string(),
            })?;
        Ok(())
    }
}

#[async_trait]
impl MetricsStore for ClickhouseMetricsStore {
    /// Batch-insert metric points into `service_metrics`.
    ///
    /// - Metric names are validated against the `[a-zA-Z0-9_.:-]` allowlist;
    ///   invalid names are skipped with a warning (the batch is not aborted).
    /// - NaN/infinite values are skipped with a warning.
    /// - Inserts are chunked at [`MAX_INSERT_BATCH`] rows per HTTP request.
    /// - `ReplacingMergeTree(_version)` deduplicates retried scrapes (the CH
    ///   analog of `ON CONFLICT DO NOTHING`).
    ///
    /// There is no separate freshness/status table: `latest_timestamp` reads
    /// `max(time)` directly.
    async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError> {
        if points.is_empty() {
            return Ok(());
        }

        for chunk in points.chunks(MAX_INSERT_BATCH) {
            // Build the valid rows first so an all-invalid chunk skips the
            // insert entirely (no empty HTTP request).
            let mut rows: Vec<ChMetricRow> = Vec::with_capacity(chunk.len());
            for p in chunk {
                // SECURITY: validate the metric name before it reaches SQL/CH.
                if validate_metric_name(&p.name).is_err() {
                    warn!(
                        metric = %p.name,
                        source_id = p.source_id,
                        "Skipping metric point: name contains characters outside the \
                         [a-zA-Z0-9_.:-] allowlist (possible injection attempt)"
                    );
                    continue;
                }

                // Enforce the Counter delta contract in debug builds.
                debug_assert!(
                    p.kind != MetricKind::Counter || p.value >= 0.0,
                    "Counter MetricPoint must carry a non-negative delta (got {})",
                    p.value
                );

                if p.value.is_nan() || p.value.is_infinite() {
                    warn!(
                        metric = %p.name,
                        value = %p.value,
                        "Skipping metric point with non-finite value"
                    );
                    continue;
                }

                rows.push(ChMetricRow::try_from_point(p)?);
            }

            if rows.is_empty() {
                continue;
            }

            let mut inserter = self
                .client
                .insert::<ChMetricRow>("service_metrics")
                .await
                .map_err(|e| MetricsError::ClickHouse {
                    operation: "write_batch (inserter setup)".to_string(),
                    reason: e.to_string(),
                })?;

            for row in &rows {
                inserter
                    .write(row)
                    .await
                    .map_err(|e| MetricsError::ClickHouse {
                        operation: "write_batch (write)".to_string(),
                        reason: e.to_string(),
                    })?;
            }

            inserter.end().await.map_err(|e| MetricsError::ClickHouse {
                operation: "write_batch (end)".to_string(),
                reason: e.to_string(),
            })?;
        }

        Ok(())
    }

    /// Bucketed `(timestamp, value)` series for the requested range.
    ///
    /// Unlike TimescaleDB (which selects raw/hourly/daily tables by range),
    /// the CH backend has only the raw table and does query-time bucketing.
    /// The trait explicitly permits this. We still coerce very wide ranges to
    /// coarser buckets to cap the result point count:
    /// - always at least [`super::clamp_step`] (no 1-minute buckets over 7 days)
    /// - range ≤ 7 days  → honor the (clamped) `filter.step`
    /// - range ≤ 90 days → at least 1-hour buckets
    /// - range > 90 days → at least 1-day buckets
    ///
    /// For gauges: `avg(value)` per bucket; for per-interval delta counters
    /// ([`super::is_delta_counter`]): `sum(value)`. For monotonic counters: per-scrape
    /// `max(value)` (collapses label-set rows to the grand total) → per-bucket
    /// `max` → `lagInFrame` delta floored at 0 (resets become 0, no first-bucket
    /// spike). `FINAL` dedups retried scrapes.
    async fn query_range(
        &self,
        filter: RangeQuery,
    ) -> Result<Vec<(DateTime<Utc>, f64)>, MetricsError> {
        // SECURITY: validate metric name before it is interpolated.
        if validate_metric_name(&filter.name).is_err() {
            warn!(
                metric_name = %filter.name,
                "query_range: metric name contains invalid characters; returning empty result"
            );
            return Ok(vec![]);
        }

        let range_duration = filter.to - filter.from;
        let seven_days = chrono::Duration::days(7);
        let ninety_days = chrono::Duration::days(90);

        // Coerce the bucket width to bound the point count:
        // - always at least clamp_step (no 1m-over-7d)
        // - range ≤ 7 days  → honor the (clamped) requested step
        // - range ≤ 90 days → at least 1-hour buckets
        // - range > 90 days → at least 1-day buckets
        let requested = super::clamp_step(range_duration, filter.step)
            .num_seconds()
            .max(1);
        let step_secs = if range_duration <= seven_days {
            requested
        } else if range_duration <= ninety_days {
            requested.max(3600)
        } else {
            requested.max(86_400)
        };

        // Determine the aggregate (fewest-label-keys) series and scope to it so
        // per-`datname` rows don't blend into the chart. `None` => no filter.
        let min_keys = self
            .min_label_key_count(filter.source_kind.as_str(), filter.source_id, &filter.name)
            .await;
        // `length(JSONExtractKeys(labels)) = N` — N is a server-derived i64, safe
        // to interpolate (it never originates from user input).
        let label_filter = match min_keys {
            Some(k) => format!(" AND length(JSONExtractKeys(labels)) = {k}"),
            None => String::new(),
        };

        // Validated metric name — the only interpolated user-controlled token.
        let nm = &filter.name;
        let from_ms = filter.from.timestamp_millis();
        let to_ms = filter.to.timestamp_millis();

        let sql = if filter.monotonic {
            // Cumulative counter: per-scrape max → per-bucket max → lagInFrame
            // delta → greatest(.,0). lagInFrame's 3rd arg = default avoids a
            // first-bucket spike. (ClickHouse 26.x deprecated neighbor().)
            //
            // NOTE: toStartOfInterval(DateTime64, INTERVAL … SECOND) returns a
            // plain `DateTime` (second resolution), so toUnixTimestamp64Milli()
            // rejects it. Buckets are always whole seconds — multiply the
            // second-precision Unix timestamp by 1000 to get the i64 ms the
            // read path expects.
            format!(
                "SELECT \
                   toInt64(toUnixTimestamp(bucket)) * 1000 AS bucket_ms, \
                   greatest(bucket_max - lagInFrame(bucket_max, 1, bucket_max) \
                            OVER (ORDER BY bucket ASC), 0) AS avg_value \
                 FROM ( \
                   SELECT toStartOfInterval(time, INTERVAL {step} SECOND) AS bucket, \
                          max(scrape_max) AS bucket_max \
                   FROM ( \
                     SELECT time, max(value) AS scrape_max \
                     FROM service_metrics FINAL \
                     WHERE source_kind = ? AND source_id = ? AND name = '{nm}' \
                       AND time >= fromUnixTimestamp64Milli(?) \
                       AND time <= fromUnixTimestamp64Milli(?){label_filter} \
                     GROUP BY time \
                   ) per_scrape \
                   GROUP BY bucket \
                   ORDER BY bucket ASC \
                 ) sub \
                 ORDER BY bucket ASC",
                step = step_secs,
                nm = nm,
                label_filter = label_filter,
            )
        } else {
            // Gauge: avg(value) per bucket; per-interval delta counters
            // (`proxy.requests*`) use sum(value) so a bucket is the real count
            // for its span. toStartOfInterval(DateTime64, … SECOND) yields a
            // second-resolution `DateTime`; * 1000 gives the i64 ms.
            let agg = super::bucket_aggregate(nm).to_lowercase();
            format!(
                "SELECT \
                   toInt64(toUnixTimestamp(toStartOfInterval(time, INTERVAL {step} SECOND))) * 1000 AS bucket_ms, \
                   {agg}(value) AS avg_value \
                 FROM service_metrics FINAL \
                 WHERE source_kind = ? AND source_id = ? AND name = '{nm}' \
                   AND time >= fromUnixTimestamp64Milli(?) \
                   AND time <= fromUnixTimestamp64Milli(?){label_filter} \
                 GROUP BY bucket_ms \
                 ORDER BY bucket_ms ASC",
                step = step_secs,
                nm = nm,
                agg = agg,
                label_filter = label_filter,
            )
        };

        let rows = self
            .client
            .query(&sql)
            .bind(filter.source_kind.as_str())
            .bind(filter.source_id)
            .bind(from_ms)
            .bind(to_ms)
            .fetch_all::<ChRangeRow>()
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "query_range".to_string(),
                reason: e.to_string(),
            })?;

        Ok(rows
            .into_iter()
            .map(|r| (ms_to_dt(r.bucket_ms), r.avg_value))
            .collect())
    }

    /// Most-recent value per metric name for a source.
    ///
    /// Orders by `(name, fewest label keys ASC, time DESC)` and takes the first
    /// row per name (`LIMIT 1 BY name`) so the instance-wide aggregate series
    /// (fewest label keys) always wins over arbitrary per-label rows. Names
    /// that have never been written are simply absent (not an error).
    async fn query_latest(
        &self,
        filter: LatestQuery,
    ) -> Result<HashMap<String, f64>, MetricsError> {
        // Build the optional name filter. Empty names => all metrics.
        let mut name_filter = String::new();
        if !filter.names.is_empty() {
            let valid: Vec<&str> = filter
                .names
                .iter()
                .filter_map(|n| match validate_metric_name(n) {
                    Ok(()) => Some(n.as_str()),
                    Err(_) => {
                        warn!(
                            metric_name = %n,
                            "query_latest: metric name contains invalid characters; excluding"
                        );
                        None
                    }
                })
                .collect();
            if valid.is_empty() {
                return Ok(HashMap::new());
            }
            // Validated names only — safe to interpolate as a quoted list.
            let list = valid
                .iter()
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(", ");
            name_filter = format!(" AND name IN ({list})");
        }

        // PERF: time-bounded so partition pruning and primary-key range
        // pruning apply — `FINAL` over an unbounded WHERE dedup-merges up to
        // the full TTL (90 days) of rows for the source on every request.
        // Anchoring on the source's own newest sample keeps values visible
        // when the scraper is paused/stale; the anchor is looked up first and
        // bound as a literal (see `NEWEST_SAMPLE_LOOKBACKS` for why a
        // subquery anchor scans the source's whole history).
        let Some(newest_ms) = self
            .newest_sample_ms(filter.source_kind.as_str(), filter.source_id)
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "query_latest".to_string(),
                reason: e.to_string(),
            })?
        else {
            return Ok(HashMap::new());
        };

        let rows = self
            .client
            .query(&latest_sql(&name_filter))
            .bind(filter.source_kind.as_str())
            .bind(filter.source_id)
            .bind(newest_ms)
            .fetch_all::<ChLatestRow>()
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "query_latest".to_string(),
                reason: e.to_string(),
            })?;

        Ok(rows.into_iter().map(|r| (r.name, r.value)).collect())
    }

    /// Most-recent value per `(name, label_value)` for the given label key.
    ///
    /// Only rows that carry the label key are considered
    /// (`JSONHas(labels, key)`), which excludes the unlabelled instance-wide
    /// aggregate. `LIMIT 1 BY name, label_value` after `ORDER BY ... time DESC`
    /// keeps the most-recent value per series.
    async fn query_latest_by_label(
        &self,
        filter: LatestByLabelQuery,
    ) -> Result<Vec<LabelledMetric>, MetricsError> {
        // SECURITY: validate the label key with the same allowlist as metric
        // names — it is passed as a bind value, but keeping the gate matches
        // the TimescaleDB store and rejects obviously bogus keys early.
        if validate_metric_name(&filter.label_key).is_err() {
            warn!(
                label_key = %filter.label_key,
                "query_latest_by_label: label key contains invalid characters; returning empty"
            );
            return Ok(Vec::new());
        }

        let valid: Vec<&str> = filter
            .names
            .iter()
            .filter_map(|n| match validate_metric_name(n) {
                Ok(()) => Some(n.as_str()),
                Err(_) => {
                    warn!(
                        metric_name = %n,
                        "query_latest_by_label: metric name contains invalid characters; excluding"
                    );
                    None
                }
            })
            .collect();
        if valid.is_empty() {
            return Ok(Vec::new());
        }
        // Validated names only.
        let list = valid
            .iter()
            .map(|n| format!("'{n}'"))
            .collect::<Vec<_>>()
            .join(", ");

        // label_key is bound (?) into JSONExtractString / JSONHas — never
        // interpolated.
        // PERF: time-bounded for the same reason as `query_latest` — see the
        // comment there for the newest-sample anchor rationale.
        let Some(newest_ms) = self
            .newest_sample_ms(filter.source_kind.as_str(), filter.source_id)
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "query_latest_by_label".to_string(),
                reason: e.to_string(),
            })?
        else {
            return Ok(Vec::new());
        };

        let rows = self
            .client
            .query(&latest_by_label_sql(&list))
            .bind(&filter.label_key)
            .bind(filter.source_kind.as_str())
            .bind(filter.source_id)
            .bind(newest_ms)
            .bind(&filter.label_key)
            .fetch_all::<ChLabelledRow>()
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "query_latest_by_label".to_string(),
                reason: e.to_string(),
            })?;

        Ok(rows
            .into_iter()
            .map(|r| LabelledMetric {
                label_value: r.label_value,
                name: r.name,
                value: r.value,
            })
            .collect())
    }

    /// Timestamp of the most-recent row for a source, or `None` if none exist.
    ///
    /// No status table (unlike TimescaleDB): `max(time)` over widening
    /// lookbacks (see [`NEWEST_SAMPLE_LOOKBACKS`]).
    async fn latest_timestamp(
        &self,
        source_kind: SourceKind,
        source_id: i32,
    ) -> Result<Option<DateTime<Utc>>, MetricsError> {
        let newest_ms = self
            .newest_sample_ms(source_kind.as_str(), source_id)
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "latest_timestamp".to_string(),
                reason: e.to_string(),
            })?;
        Ok(newest_ms.map(ms_to_dt))
    }

    /// Prune rows older than `older_than`.
    ///
    /// The raw table has a native TTL (90 days) that handles retention via
    /// cheap partition drops at merge time, so an expensive hourly
    /// `ALTER ... DELETE` is unnecessary. This is a no-op that returns `Ok(0)`;
    /// the hourly task simply logs the count. The trait contract (`u64`) holds.
    async fn prune(&self, _older_than: DateTime<Utc>) -> Result<u64, MetricsError> {
        Ok(0)
    }
}

impl ClickhouseMetricsStore {
    /// Minimum label-key count across the recent rows of a metric, or `None`
    /// on error / no rows.
    ///
    /// The instance-wide aggregate row has the FEWEST label keys; `query_range`
    /// scopes the chart to that series so per-`datname` rows don't blend in.
    /// Bounded to the most-recent 64 rows (a single scrape writes every series
    /// at once, so this window contains them all). Returns `None` on error so
    /// the caller falls back to an unfiltered query rather than charting
    /// nothing.
    async fn min_label_key_count(
        &self,
        source_kind: &str,
        source_id: i32,
        name: &str,
    ) -> Option<i64> {
        if validate_metric_name(name).is_err() {
            return None;
        }

        #[derive(::clickhouse::Row, Deserialize, Debug)]
        struct MinKeysRow {
            min_keys: i64,
        }

        // Validated metric name interpolated; source_kind/source_id bound.
        // Time-bounded so partition/primary-key pruning applies (see
        // query_latest); without it the ORDER BY time DESC sort reads the
        // metric's full history before the LIMIT.
        let newest_ms = match self.newest_sample_ms(source_kind, source_id).await {
            Ok(Some(ms)) => ms,
            Ok(None) => return None,
            Err(error) => {
                warn!(
                    operation = "min_label_key_count",
                    source_kind,
                    source_id,
                    metric_name = name,
                    reason = %error,
                    "ClickHouse newest-sample query failed; range query will use all label series"
                );
                return None;
            }
        };

        match self
            .client
            .query(&min_label_key_count_sql(name))
            .bind(source_kind)
            .bind(source_id)
            .bind(newest_ms)
            .fetch_one::<MinKeysRow>()
            .await
        {
            // No rows => min over empty set is 0 in CH; that still yields a
            // harmless `= 0` filter. We only treat a query error as `None`.
            Ok(r) => Some(r.min_keys),
            Err(error) => {
                warn!(
                    operation = "min_label_key_count",
                    source_kind,
                    source_id,
                    metric_name = name,
                    reason = %error,
                    "ClickHouse label-count query failed; range query will use all label series"
                );
                None
            }
        }
    }

    /// Unix-ms of a source's newest sample, or `None` if it has no rows.
    ///
    /// Tries [`NEWEST_SAMPLE_LOOKBACKS`] narrowest first so a live source
    /// never pays for its full history. `FINAL` is unnecessary: the newest
    /// timestamp is identical pre/post merge. An empty set yields
    /// `max_ms = 0`, which means "widen the lookback".
    async fn newest_sample_ms(
        &self,
        source_kind: &str,
        source_id: i32,
    ) -> Result<Option<i64>, ::clickhouse::error::Error> {
        for lookback in NEWEST_SAMPLE_LOOKBACKS {
            let row = self
                .client
                .query(&newest_sample_sql(lookback))
                .bind(source_kind)
                .bind(source_id)
                .fetch_one::<ChMaxTimeRow>()
                .await?;
            if row.max_ms != 0 {
                return Ok(Some(row.max_ms));
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MetricKind, SourceKind};
    use std::collections::HashMap;

    async fn setup_clickhouse_store(
    ) -> Option<(ClickhouseMetricsStore, Box<dyn std::any::Any + Send>)> {
        use testcontainers::{
            core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };

        let image = GenericImage::new("clickhouse/clickhouse-server", "26.2.5")
            .with_exposed_port(ContainerPort::Tcp(8123))
            .with_wait_for(WaitFor::http(
                HttpWaitStrategy::new("/ping")
                    .with_port(ContainerPort::Tcp(8123))
                    .with_expected_status_code(200u16),
            ))
            .with_env_var("CLICKHOUSE_DB", "temps_metrics_test")
            .with_env_var("CLICKHOUSE_PASSWORD", "test");

        let container = match image.start().await {
            Ok(container) => container,
            Err(error) => {
                eprintln!("Skipping ClickHouse metrics test: cannot start container ({error})");
                return None;
            }
        };
        let host_port = match container.get_host_port_ipv4(8123).await {
            Ok(port) => port,
            Err(error) => {
                eprintln!("Skipping ClickHouse metrics test: cannot get host port ({error})");
                return None;
            }
        };

        let store = ClickhouseMetricsStore::new(ClickHouseMetricsConfig::new(
            format!("http://127.0.0.1:{host_port}"),
            "temps_metrics_test",
            "default",
            "test",
        ));

        let mut last_error = String::new();
        for _ in 0..30 {
            match store.client().query("SELECT 1").execute().await {
                Ok(()) => {
                    last_error.clear();
                    break;
                }
                Err(error) => {
                    last_error = error.to_string();
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        if !last_error.is_empty() {
            eprintln!("Skipping ClickHouse metrics test: server never became ready ({last_error})");
            return None;
        }

        crate::store::clickhouse_migrations::apply_migrations(store.client(), "temps_metrics_test")
            .await
            .expect("apply metrics ClickHouse migrations");

        Some((store, Box::new(container)))
    }

    fn point(name: &str, value: f64, kind: MetricKind) -> MetricPoint {
        let mut labels = HashMap::new();
        labels.insert("engine".to_string(), "postgres".to_string());
        MetricPoint {
            time: Utc.timestamp_opt(1_717_200_000, 0).single().unwrap(),
            source_kind: SourceKind::Database,
            source_id: 7,
            name: name.to_string(),
            value,
            kind,
            engine: Some("postgres".to_string()),
            environment: Some("production".to_string()),
            node_id: Some(3),
            labels,
        }
    }

    #[tokio::test]
    async fn clickhouse_range_uses_the_fewest_label_key_series() {
        let Some((store, _container)) = setup_clickhouse_store().await else {
            return;
        };

        let timestamp = Utc::now();
        let mut aggregate = point("pg.connections_active", 10.0, MetricKind::Gauge);
        aggregate.time = timestamp;
        aggregate.labels.clear();

        let mut per_database = point("pg.connections_active", 100.0, MetricKind::Gauge);
        per_database.time = timestamp;
        per_database.labels.clear();
        per_database.labels.insert("datname".into(), "app".into());

        store
            .write_batch(vec![aggregate, per_database])
            .await
            .expect("insert aggregate and labelled metric fixtures");

        assert_eq!(
            store
                .min_label_key_count("database", 7, "pg.connections_active")
                .await,
            Some(0),
            "ClickHouse UInt64 label counts must decode into the signed query filter"
        );

        let values = store
            .query_range(RangeQuery {
                source_kind: SourceKind::Database,
                source_id: 7,
                name: "pg.connections_active".to_string(),
                from: timestamp - chrono::Duration::minutes(1),
                to: timestamp + chrono::Duration::minutes(1),
                step: chrono::Duration::minutes(1),
                monotonic: false,
            })
            .await
            .expect("query aggregate metric series");
        assert_eq!(values.len(), 1);
        assert_eq!(
            values[0].1, 10.0,
            "labelled rows must not blend into aggregate series"
        );
    }

    /// Seed `days` of history for one source: `names` series, one sample
    /// every 15 s, newest at `newest_age_secs` before now. The value of every
    /// series' newest sample is 1000.0; older samples count down from there.
    async fn seed_history(
        store: &ClickhouseMetricsStore,
        source_kind: &str,
        source_id: i32,
        names: u64,
        days: u64,
        newest_age_secs: u64,
    ) {
        let samples = days * 86_400 / 15;
        store
            .client()
            .query(&format!(
                "INSERT INTO service_metrics (time, source_kind, source_id, name, value, kind, labels) \
                 SELECT now64(3) - toIntervalSecond({newest_age_secs} + intDiv(number, {names}) * 15), \
                        ?, ?, concat('node.series_', toString(number % {names})), \
                        1000.0 - intDiv(number, {names}), 'gauge', '{{}}' \
                 FROM numbers({total})",
                total = samples * names,
            ))
            .bind(source_kind)
            .bind(source_id)
            .execute()
            .await
            .expect("seed metric history");
    }

    /// Rows ClickHouse read for every `service_metrics` query that finished
    /// since `since_us` (server clock, microseconds).
    async fn rows_read_since(store: &ClickhouseMetricsStore, since_us: i64) -> u64 {
        #[derive(::clickhouse::Row, Deserialize)]
        struct ReadRows {
            rows: u64,
        }
        store
            .client()
            .query("SYSTEM FLUSH LOGS")
            .execute()
            .await
            .expect("flush query log");
        store
            .client()
            .query(
                "SELECT toUInt64(sum(read_rows)) AS rows FROM system.query_log \
                 WHERE type = 'QueryFinish' \
                   AND toUnixTimestamp64Micro(event_time_microseconds) >= ? \
                   AND query LIKE '%FROM service_metrics%' \
                   AND query NOT LIKE '%system.query_log%'",
            )
            .bind(since_us)
            .fetch_one::<ReadRows>()
            .await
            .expect("read query log")
            .rows
    }

    async fn server_now_us(store: &ClickhouseMetricsStore) -> i64 {
        #[derive(::clickhouse::Row, Deserialize)]
        struct Now {
            us: i64,
        }
        store
            .client()
            .query("SELECT toUnixTimestamp64Micro(now64(6)) AS us")
            .fetch_one::<Now>()
            .await
            .expect("read server clock")
            .us
    }

    /// Regression for #1616: the latest-value reads anchored their time bound
    /// on an unbounded `max(time)` subquery, which reads the `time` column of
    /// every row the source ever wrote (the sort key puts `name` before
    /// `time`), so a node poll scanned the full 90-day history every time.
    #[tokio::test]
    async fn clickhouse_latest_reads_do_not_scan_full_source_history() {
        let Some((store, _container)) = setup_clickhouse_store().await else {
            return;
        };

        const NAMES: u64 = 20;
        const DAYS: u64 = 30;
        seed_history(&store, "node", 0, NAMES, DAYS, 0).await;
        // Merge into one part per month, as an aged production table is.
        store
            .client()
            .query("OPTIMIZE TABLE service_metrics FINAL")
            .execute()
            .await
            .expect("merge seeded parts");
        let history_rows = NAMES * DAYS * 86_400 / 15;

        let wanted = vec!["node.series_0".to_string(), "node.series_7".to_string()];
        let since = server_now_us(&store).await;
        let latest = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Node,
                source_id: 0,
                names: wanted.clone(),
            })
            .await
            .expect("query latest node metrics");
        let read = rows_read_since(&store, since).await;

        assert_eq!(latest.len(), 2, "both requested series present: {latest:?}");
        for name in &wanted {
            assert_eq!(latest.get(name), Some(&1000.0), "newest sample of {name}");
        }
        // Pruned reads touch a granule or two (8192 rows) per series; the bug
        // read every row the source ever wrote.
        assert!(
            read < history_rows / 5,
            "query_latest read {read} of {history_rows} history rows"
        );

        let since = server_now_us(&store).await;
        let all = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Node,
                source_id: 0,
                names: vec![],
            })
            .await
            .expect("query every latest node metric");
        let read = rows_read_since(&store, since).await;
        assert_eq!(all.len() as u64, NAMES);
        assert!(
            read < history_rows / 5,
            "query_latest (all names) read {read} of {history_rows} history rows"
        );

        let since = server_now_us(&store).await;
        assert_eq!(
            store.min_label_key_count("node", 0, "node.series_3").await,
            Some(0)
        );
        let read = rows_read_since(&store, since).await;
        assert!(
            read < history_rows / 5,
            "min_label_key_count read {read} of {history_rows} history rows"
        );

        let since = server_now_us(&store).await;
        let newest = store
            .latest_timestamp(SourceKind::Node, 0)
            .await
            .expect("latest node timestamp")
            .expect("node has samples");
        let read = rows_read_since(&store, since).await;
        assert!((Utc::now() - newest).num_minutes() < 5, "newest = {newest}");
        assert!(
            read < history_rows / 5,
            "latest_timestamp read {read} of {history_rows} history rows"
        );
    }

    /// The bound is anchored on the source's own newest sample, not on now(),
    /// so a paused scraper still shows its last values; a source with no rows
    /// returns nothing.
    #[tokio::test]
    async fn clickhouse_latest_reads_still_see_a_stale_source() {
        let Some((store, _container)) = setup_clickhouse_store().await else {
            return;
        };

        // Last sample 3 days ago: past the 15-minute and 1-day lookbacks.
        seed_history(&store, "node", 5, 3, 2, 3 * 86_400).await;

        let latest = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Node,
                source_id: 5,
                names: vec![],
            })
            .await
            .expect("query latest for a stale node");
        assert_eq!(latest.len(), 3, "{latest:?}");
        assert!(latest.values().all(|v| *v == 1000.0), "{latest:?}");

        let newest = store
            .latest_timestamp(SourceKind::Node, 5)
            .await
            .expect("latest stale node timestamp")
            .expect("stale node has samples");
        let age = Utc::now() - newest;
        assert!(
            age.num_hours() >= 71 && age.num_hours() <= 73,
            "newest = {newest}"
        );
        assert_eq!(
            store.min_label_key_count("node", 5, "node.series_1").await,
            Some(0)
        );

        let labelled = store
            .query_latest_by_label(LatestByLabelQuery {
                source_kind: SourceKind::Node,
                source_id: 5,
                names: vec!["node.series_1".to_string()],
                label_key: "mount".to_string(),
            })
            .await
            .expect("query latest by label for a stale node");
        assert!(labelled.is_empty(), "seeded rows carry no labels");

        let empty = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Node,
                source_id: 99,
                names: vec![],
            })
            .await
            .expect("query latest for a node with no rows");
        assert!(empty.is_empty());
        assert_eq!(
            store
                .latest_timestamp(SourceKind::Node, 99)
                .await
                .expect("latest timestamp for a node with no rows"),
            None
        );
    }

    /// #1616: the latest-value reads must not derive their time bound from a
    /// subquery. `max(time)` per source reads the source's whole history, and
    /// a bound ClickHouse cannot see as a constant prunes nothing.
    #[test]
    fn latest_value_sql_takes_the_time_bound_as_a_literal() {
        let cases = [
            (
                "query_latest",
                latest_sql(" AND name IN ('node.cpu_percent')"),
            ),
            ("query_latest (all)", latest_sql("")),
            (
                "query_latest_by_label",
                latest_by_label_sql("'pg.database_size_bytes'"),
            ),
            (
                "min_label_key_count",
                min_label_key_count_sql("pg.connections_active"),
            ),
        ];
        for (operation, sql) in cases {
            assert!(
                !sql.contains("max(time)"),
                "{operation} must not anchor on a max(time) subquery: {sql}"
            );
            assert!(
                sql.contains("time > fromUnixTimestamp64Milli(?) - INTERVAL 15 MINUTE"),
                "{operation} must bound time from the bound newest sample: {sql}"
            );
        }
    }

    #[test]
    fn newest_sample_lookups_are_bounded_until_the_last_tier() {
        let (last, bounded) = NEWEST_SAMPLE_LOOKBACKS
            .split_last()
            .expect("at least one lookback");
        assert_eq!(*last, None, "the last tier keeps stale sources visible");
        assert_eq!(
            bounded.first().copied().flatten(),
            Some(LATEST_WINDOW),
            "a live source is answered from the latest window itself"
        );
        for lookback in bounded {
            let sql = newest_sample_sql(*lookback);
            let interval = lookback.expect("only the last tier is unbounded");
            assert!(
                sql.ends_with(&format!(" AND time > now64(3) - {interval}")),
                "{sql}"
            );
        }
        assert!(!newest_sample_sql(None).contains("time >"));
    }

    #[test]
    fn config_debug_masks_password() {
        let cfg =
            ClickHouseMetricsConfig::new("http://localhost:8123", "otel", "temps", "super-secret");
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("***"));
        assert!(!dbg.contains("super-secret"));
        assert!(dbg.contains("otel"));
    }

    #[test]
    fn configured_compression_reaches_the_metrics_client() {
        use temps_clickhouse::ClickHouseCompression;

        let build = |mode| {
            let store = ClickhouseMetricsStore::new(
                ClickHouseMetricsConfig::new("http://127.0.0.1:1", "otel", "temps", "pw")
                    .with_compression(mode),
            );
            format!("{:?}", store.client())
        };

        let lz4 = build(ClickHouseCompression::Lz4);
        assert!(lz4.contains("compression: Lz4"), "{lz4}");
        assert!(
            lz4.contains("\"network_compression_method\": \"lz4\""),
            "{lz4}"
        );

        let none = build(ClickHouseCompression::None);
        assert!(none.contains("compression: None"), "{none}");
        assert!(!none.contains("network_compression_method"), "{none}");
    }

    #[test]
    fn metric_kind_to_str_maps_both() {
        assert_eq!(metric_kind_to_str(&MetricKind::Gauge), "gauge");
        assert_eq!(metric_kind_to_str(&MetricKind::Counter), "counter");
    }

    #[test]
    fn ms_to_dt_roundtrips() {
        let ms = 1_717_200_000_123_i64;
        let dt = ms_to_dt(ms);
        assert_eq!(dt.timestamp_millis(), ms);
    }

    #[test]
    fn ms_to_dt_zero_is_epoch() {
        assert_eq!(ms_to_dt(0).timestamp_millis(), 0);
    }

    #[test]
    fn try_from_point_maps_all_fields() {
        let p = point("pg.connections_active", 42.0, MetricKind::Gauge);
        let row = ChMetricRow::try_from_point(&p).expect("conversion should succeed");
        assert_eq!(row.time, p.time.timestamp_millis());
        assert_eq!(row.source_kind, "database");
        assert_eq!(row.source_id, 7);
        assert_eq!(row.name, "pg.connections_active");
        assert_eq!(row.value, 42.0);
        assert_eq!(row.kind, "gauge");
        assert_eq!(row.engine, "postgres");
        assert_eq!(row.environment, "production");
        assert_eq!(row.node_id, Some(3));
        // labels round-trip back to the same map.
        let labels: HashMap<String, String> =
            serde_json::from_str(&row.labels).expect("labels must be valid json");
        assert_eq!(labels.get("engine"), Some(&"postgres".to_string()));
        assert!(row._version > 0);
    }

    #[test]
    fn labels_serialize_deterministically_regardless_of_insertion_order() {
        // `labels` is in the ClickHouse ORDER BY (the ReplacingMergeTree dedup
        // key). Two points with the SAME logical labels inserted in different
        // order must serialize to identical bytes, or the same series splits
        // into two rows that never dedup. This guards the BTreeMap-sort fix.
        let mut a = point("pg.x", 1.0, MetricKind::Gauge);
        a.labels = HashMap::new();
        a.labels.insert("datname".into(), "prod".into());
        a.labels.insert("container".into(), "web".into());
        a.labels.insert("zzz".into(), "last".into());

        let mut b = point("pg.x", 1.0, MetricKind::Gauge);
        b.labels = HashMap::new();
        // Same pairs, inserted in a different order.
        b.labels.insert("zzz".into(), "last".into());
        b.labels.insert("container".into(), "web".into());
        b.labels.insert("datname".into(), "prod".into());

        let ra = ChMetricRow::try_from_point(&a).unwrap();
        let rb = ChMetricRow::try_from_point(&b).unwrap();
        assert_eq!(
            ra.labels, rb.labels,
            "identical labels must serialize identically; got {} vs {}",
            ra.labels, rb.labels
        );
        // And the output is key-sorted (BTreeMap order).
        assert_eq!(
            ra.labels,
            r#"{"container":"web","datname":"prod","zzz":"last"}"#
        );
    }

    #[test]
    fn try_from_point_none_options_become_empty_sentinels() {
        let mut p = point("redis.connected_clients", 3.0, MetricKind::Gauge);
        p.engine = None;
        p.environment = None;
        p.node_id = None;
        p.labels = HashMap::new();
        let row = ChMetricRow::try_from_point(&p).expect("conversion should succeed");
        assert_eq!(row.engine, "");
        assert_eq!(row.environment, "");
        assert_eq!(row.node_id, None);
        assert_eq!(row.labels, "{}");
    }

    #[test]
    fn try_from_point_counter_kind_string() {
        let p = point("pg.xact_commit", 10.0, MetricKind::Counter);
        let row = ChMetricRow::try_from_point(&p).expect("conversion should succeed");
        assert_eq!(row.kind, "counter");
    }

    // ── Name validation is the security boundary for the CH store too. ──────
    // These mirror the TimescaleDB store's tests so the CH write/read paths
    // reject the same payloads before any name reaches SQL.

    #[test]
    fn validate_metric_name_accepts_valid() {
        assert!(validate_metric_name("pg.connections_active").is_ok());
        assert!(validate_metric_name("my-service:metric_v2").is_ok());
        assert!(validate_metric_name("A-Z_0.9:metric").is_ok());
    }

    #[test]
    fn validate_metric_name_rejects_injection_and_empty() {
        assert!(validate_metric_name("").is_err());
        assert!(validate_metric_name("'; DROP TABLE service_metrics; --").is_err());
        assert!(validate_metric_name("name with space").is_err());
        assert!(validate_metric_name("name;semicolon").is_err());
        assert!(validate_metric_name("name\nnewline").is_err());
    }

    #[tokio::test]
    async fn write_batch_empty_is_noop_ok() {
        // No client connection is touched for an empty batch — the early
        // return runs before any I/O, so this passes without a live CH.
        let store = ClickhouseMetricsStore::new(ClickHouseMetricsConfig::new(
            "http://127.0.0.1:1", // unreachable on purpose
            "otel",
            "temps",
            "temps_dev",
        ));
        assert!(store.write_batch(vec![]).await.is_ok());
    }

    #[tokio::test]
    async fn query_range_invalid_name_returns_empty_without_io() {
        // An invalid metric name short-circuits to Ok(vec![]) BEFORE any CH
        // round-trip, so an unreachable URL still yields success.
        let store = ClickhouseMetricsStore::new(ClickHouseMetricsConfig::new(
            "http://127.0.0.1:1",
            "otel",
            "temps",
            "temps_dev",
        ));
        let res = store
            .query_range(RangeQuery {
                source_kind: SourceKind::Database,
                source_id: 1,
                name: "bad name; DROP TABLE service_metrics".to_string(),
                from: Utc::now() - chrono::Duration::hours(1),
                to: Utc::now(),
                step: chrono::Duration::seconds(30),
                monotonic: false,
            })
            .await;
        assert_eq!(res.expect("invalid name => empty, not error"), vec![]);
    }

    #[tokio::test]
    async fn query_latest_all_invalid_names_returns_empty_without_io() {
        let store = ClickhouseMetricsStore::new(ClickHouseMetricsConfig::new(
            "http://127.0.0.1:1",
            "otel",
            "temps",
            "temps_dev",
        ));
        let res = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Database,
                source_id: 1,
                names: vec!["bad;name".to_string(), "also bad".to_string()],
            })
            .await
            .expect("all-invalid names => empty map, not error");
        assert!(res.is_empty());
    }

    #[tokio::test]
    async fn query_latest_by_label_invalid_key_returns_empty_without_io() {
        let store = ClickhouseMetricsStore::new(ClickHouseMetricsConfig::new(
            "http://127.0.0.1:1",
            "otel",
            "temps",
            "temps_dev",
        ));
        let res = store
            .query_latest_by_label(LatestByLabelQuery {
                source_kind: SourceKind::Database,
                source_id: 1,
                names: vec!["pg.database_size_bytes".to_string()],
                label_key: "bad key; --".to_string(),
            })
            .await
            .expect("invalid label key => empty vec, not error");
        assert!(res.is_empty());
    }

    #[tokio::test]
    async fn prune_is_noop_zero() {
        // TTL handles retention; prune returns Ok(0) and touches no I/O.
        let store = ClickhouseMetricsStore::new(ClickHouseMetricsConfig::new(
            "http://127.0.0.1:1",
            "otel",
            "temps",
            "temps_dev",
        ));
        assert_eq!(store.prune(Utc::now()).await.expect("prune is a no-op"), 0);
    }
}
