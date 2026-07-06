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
}

// Manual Debug that masks the password so it can never leak into logs.
impl std::fmt::Debug for ClickHouseMetricsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseMetricsConfig")
            .field("url", &self.url)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"***")
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
        }
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
        let client = ::clickhouse::Client::default()
            .with_url(&config.url)
            .with_database(&config.database)
            .with_user(&config.user)
            .with_password(&config.password);
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
    /// - range ≤ 7 days  → honor `filter.step`
    /// - range ≤ 90 days → at least 1-hour buckets
    /// - range > 90 days → at least 1-day buckets
    ///
    /// For gauges: `avg(value)` per bucket. For monotonic counters: per-scrape
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

        // Coerce the bucket width for wide ranges to bound the point count.
        let requested = filter.step.num_seconds().max(1);
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
                   toUnixTimestamp(bucket) * 1000 AS bucket_ms, \
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
            // Gauge: avg(value) per bucket. toStartOfInterval(DateTime64, … SECOND)
            // yields a second-resolution `DateTime`; * 1000 gives the i64 ms.
            format!(
                "SELECT \
                   toUnixTimestamp(toStartOfInterval(time, INTERVAL {step} SECOND)) * 1000 AS bucket_ms, \
                   avg(value) AS avg_value \
                 FROM service_metrics FINAL \
                 WHERE source_kind = ? AND source_id = ? AND name = '{nm}' \
                   AND time >= fromUnixTimestamp64Milli(?) \
                   AND time <= fromUnixTimestamp64Milli(?){label_filter} \
                 GROUP BY bucket_ms \
                 ORDER BY bucket_ms ASC",
                step = step_secs,
                nm = nm,
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

        let sql = format!(
            "SELECT name, value \
             FROM service_metrics FINAL \
             WHERE source_kind = ? AND source_id = ?{name_filter} \
             ORDER BY name, length(JSONExtractKeys(labels)) ASC, time DESC \
             LIMIT 1 BY name",
            name_filter = name_filter,
        );

        let rows = self
            .client
            .query(&sql)
            .bind(filter.source_kind.as_str())
            .bind(filter.source_id)
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
        let sql = format!(
            "SELECT name, JSONExtractString(labels, ?) AS label_value, value \
             FROM service_metrics FINAL \
             WHERE source_kind = ? AND source_id = ? \
               AND name IN ({list}) \
               AND JSONHas(labels, ?) \
             ORDER BY name, label_value, time DESC \
             LIMIT 1 BY name, label_value",
            list = list,
        );

        let rows = self
            .client
            .query(&sql)
            .bind(&filter.label_key)
            .bind(filter.source_kind.as_str())
            .bind(filter.source_id)
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
    /// No status table (unlike TimescaleDB): `max(time)` directly. An empty set
    /// yields `max_ms = 0`, which we map to `None`.
    async fn latest_timestamp(
        &self,
        source_kind: SourceKind,
        source_id: i32,
    ) -> Result<Option<DateTime<Utc>>, MetricsError> {
        // FINAL is unnecessary for a pure max(time) and would add cost; the
        // newest timestamp is identical pre/post merge.
        let row = self
            .client
            .query(
                "SELECT toUnixTimestamp64Milli(max(time)) AS max_ms \
                 FROM service_metrics \
                 WHERE source_kind = ? AND source_id = ?",
            )
            .bind(source_kind.as_str())
            .bind(source_id)
            .fetch_one::<ChMaxTimeRow>()
            .await
            .map_err(|e| MetricsError::ClickHouse {
                operation: "latest_timestamp".to_string(),
                reason: e.to_string(),
            })?;

        if row.max_ms == 0 {
            Ok(None)
        } else {
            Ok(Some(ms_to_dt(row.max_ms)))
        }
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
        let sql = format!(
            "SELECT min(k) AS min_keys FROM ( \
               SELECT length(JSONExtractKeys(labels)) AS k \
               FROM service_metrics \
               WHERE source_kind = ? AND source_id = ? AND name = '{nm}' \
               ORDER BY time DESC LIMIT 64 \
             )",
            nm = name,
        );

        match self
            .client
            .query(&sql)
            .bind(source_kind)
            .bind(source_id)
            .fetch_one::<MinKeysRow>()
            .await
        {
            // No rows => min over empty set is 0 in CH; that still yields a
            // harmless `= 0` filter. We only treat a query error as `None`.
            Ok(r) => Some(r.min_keys),
            Err(_) => None,
        }
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
