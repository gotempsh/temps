// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

use crate::error::MetricsError;
use crate::store::{
    LabelledMetric, LatestByLabelQuery, LatestQuery, MetricKind, MetricPoint, MetricsStore,
    RangeQuery, SourceKind,
};

/// Maximum rows per INSERT statement. Each chunk is shipped as a single JSONB
/// bind parameter; capping it keeps one statement's payload (~300 bytes/row,
/// so ~150 KB at 500 rows) well inside safe limits and bounds the blast radius
/// of an all-or-nothing failure.
const BATCH_SIZE: usize = 500;

/// Lookback window for "latest value" queries, anchored to the source's
/// `service_metrics_status.last_received_at`. Wide enough to cover many scrape
/// cycles (scrapes run every 10-30 s) plus clock skew between the scraper and
/// the database, while keeping the scan to the most recent hypertable chunk
/// instead of the source's full retention history.
///
/// Compile-time constant: it is the only value spliced into SQL text.
const LATEST_WINDOW: &str = "15 minutes";

/// Multi-row insert into `service_metrics`.
///
/// SECURITY(metrics-security-1): every value is carried by the single `$1`
/// JSONB bind parameter and expanded server-side by `jsonb_to_recordset`, so
/// no metric name, label, engine or environment string is ever spliced into
/// SQL text. The statement text is constant, which also keeps it to a single
/// entry in the per-connection prepared-statement cache regardless of batch
/// size.
const INSERT_METRICS_SQL: &str = "INSERT INTO service_metrics \
     (time, source_kind, source_id, name, value, engine, environment, node_id, labels) \
     SELECT r.time, r.source_kind, r.source_id, r.name, r.value, r.engine, r.environment, \
            r.node_id, COALESCE(r.labels, '{}'::jsonb) \
     FROM jsonb_to_recordset($1::jsonb) AS r( \
         time timestamptz, source_kind text, source_id integer, name text, \
         value double precision, engine text, environment text, node_id integer, labels jsonb) \
     ON CONFLICT DO NOTHING";

/// Per-source freshness upsert, parameterised the same way as
/// [`INSERT_METRICS_SQL`].
const UPSERT_STATUS_SQL: &str = "INSERT INTO service_metrics_status \
     (source_kind, source_id, last_received_at) \
     SELECT r.source_kind, r.source_id, r.last_received_at \
     FROM jsonb_to_recordset($1::jsonb) AS r( \
         source_kind text, source_id integer, last_received_at timestamptz) \
     ON CONFLICT (source_kind, source_id) DO UPDATE \
     SET last_received_at = GREATEST(service_metrics_status.last_received_at, \
                                     EXCLUDED.last_received_at)";

/// TimescaleDB-backed implementation of [`MetricsStore`].
///
/// Writes are chunked into batches of at most [`BATCH_SIZE`] rows. Reads
/// select the correct table (raw / hourly / daily) based on the query range so
/// TimescaleDB chunk exclusion is always active.
///
/// SECURITY(metrics-security-1): every statement this store issues passes
/// caller-supplied values (metric names, label keys/values, engine,
/// environment, timestamps, ids) as `$N` bind parameters. The only text
/// spliced into SQL is compile-time constants and positional placeholders, so
/// correctness no longer depends on string escaping or on the server's
/// `standard_conforming_strings` setting. The metric-name allowlist
/// ([`validate_metric_name`]) is kept as defence in depth and as an input
/// contract for alert rules.
pub struct TimescaleMetricsStore {
    db: Arc<DatabaseConnection>,
}

/// Build a Postgres statement with positional bind values.
fn pg_statement(sql: impl Into<String>, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}

/// `$start, $start+1, …` placeholders for an `IN (…)` list of `count` values.
fn placeholders(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Keep only the names that pass [`validate_metric_name`], logging the rest.
fn valid_metric_names<'a>(names: &'a [String], caller: &str) -> Vec<&'a str> {
    names
        .iter()
        .filter_map(|n| match validate_metric_name(n) {
            Ok(()) => Some(n.as_str()),
            Err(_) => {
                warn!(
                    metric_name = %n,
                    caller,
                    "metric name contains characters outside the [a-zA-Z0-9_.:-] \
                     allowlist; excluding from query"
                );
                None
            }
        })
        .collect()
}

/// Build the parameterised batch-insert statement for `service_metrics`.
///
/// Returns `None` when no point survives validation (nothing to insert).
/// Points with a name outside the allowlist or a non-finite value are dropped
/// with a warning rather than failing the whole batch.
fn build_insert_statement(points: &[MetricPoint]) -> Option<Statement> {
    let mut rows: Vec<serde_json::Value> = Vec::with_capacity(points.len());

    for p in points {
        // SECURITY(metrics-security-1): metric names on the OTLP ingest path
        // come straight off the wire. They are bound, not interpolated, but
        // the allowlist is still enforced so the stored data matches what the
        // read path and alert rules accept.
        if validate_metric_name(&p.name).is_err() {
            warn!(
                metric = %p.name,
                source_id = p.source_id,
                "Skipping metric point: name contains characters outside the \
                 [a-zA-Z0-9_.:-] allowlist"
            );
            continue;
        }

        // Enforce Counter delta contract in debug builds.
        // (Issue 8: counter delta loss on restart is a caller
        //  responsibility; this assert validates the invariant.)
        debug_assert!(
            p.kind != MetricKind::Counter || p.value >= 0.0,
            "Counter MetricPoint must carry a non-negative delta (got {})",
            p.value
        );

        if !p.value.is_finite() {
            warn!(
                metric = %p.name,
                value = %p.value,
                "Skipping metric point with non-finite value"
            );
            continue;
        }

        rows.push(serde_json::json!({
            // Microsecond precision with a UTC 'Z' suffix — TIMESTAMPTZ only
            // stores microseconds.
            "time": p.time.to_rfc3339_opts(SecondsFormat::Micros, true),
            "source_kind": p.source_kind.as_str(),
            "source_id": p.source_id,
            "name": p.name,
            "value": p.value,
            "engine": p.engine,
            "environment": p.environment,
            "node_id": p.node_id,
            "labels": p.labels,
        }));
    }

    if rows.is_empty() {
        return None;
    }

    // FIXME(metrics-scale): Issue 6 (Correctness Review) — `ON CONFLICT DO NOTHING`
    // requires a UNIQUE constraint on `(time, source_kind, source_id, name)` to
    // actually suppress duplicates.  The current migration only creates a plain
    // B-tree index on those columns, not a UNIQUE constraint.  Without a UNIQUE
    // constraint, `ON CONFLICT DO NOTHING` is a no-op: PostgreSQL accepts all
    // rows including duplicates, which causes double-counting in continuous
    // aggregates when `write_batch` is retried after a transient failure.
    //
    // Fix options (before GA):
    //   a) Add `UNIQUE (time, source_kind, source_id, name)` to the migration.
    //      Note: unique indexes are not compressed by TimescaleDB; at high write
    //      rates this imposes significant index maintenance overhead.
    //   b) Remove `ON CONFLICT DO NOTHING` and rely on the scraper's `in_flight`
    //      HashSet (already in place) to prevent duplicate scrapes.
    //   c) Migrate to COPY-based bulk inserts which never produce duplicates in
    //      normal operation.
    Some(pg_statement(
        INSERT_METRICS_SQL,
        vec![serde_json::Value::Array(rows).into()],
    ))
}

/// Build the parameterised freshness upsert for every distinct source in
/// `points`, or `None` for an empty batch.
fn build_status_statement(points: &[MetricPoint]) -> Option<Statement> {
    let mut latest_by_source: HashMap<(&'static str, i32), DateTime<Utc>> = HashMap::new();
    for p in points {
        latest_by_source
            .entry((p.source_kind.as_str(), p.source_id))
            .and_modify(|t| {
                if p.time > *t {
                    *t = p.time;
                }
            })
            .or_insert(p.time);
    }
    if latest_by_source.is_empty() {
        return None;
    }
    let rows: Vec<serde_json::Value> = latest_by_source
        .into_iter()
        .map(|((sk, sid), t)| {
            serde_json::json!({
                "source_kind": sk,
                "source_id": sid,
                "last_received_at": t.to_rfc3339_opts(SecondsFormat::Micros, true),
            })
        })
        .collect();
    Some(pg_statement(
        UPSERT_STATUS_SQL,
        vec![serde_json::Value::Array(rows).into()],
    ))
}

/// Build the bucketed range query for `filter`.
///
/// `min_keys` scopes the series to rows with exactly that many label keys
/// (see [`TimescaleMetricsStore::query_range`]). The caller must have
/// validated `filter.name`.
fn build_range_statement(filter: &RangeQuery, min_keys: Option<i64>) -> Statement {
    let range_duration = filter.to - filter.from;
    // Use Duration constants to avoid num_hours() integer truncation.
    let seven_days = chrono::Duration::days(7);
    let ninety_days = chrono::Duration::days(90);

    // $1 source_kind, $2 source_id, $3 name, $4 from, $5 to
    let mut values: Vec<Value> = vec![
        filter.source_kind.as_str().into(),
        filter.source_id.into(),
        filter.name.clone().into(),
        filter.from.into(),
        filter.to.into(),
    ];
    let label_filter = match min_keys {
        Some(k) => {
            values.push(k.into());
            format!(
                " AND (SELECT count(*) FROM jsonb_object_keys(labels)) = ${}",
                values.len()
            )
        }
        None => String::new(),
    };

    let sql = if range_duration <= seven_days {
        // Raw table — use time_bucket with the requested step, coarsened so a
        // 7-day window cannot emit 1-minute buckets.
        let step_secs = super::clamp_step(range_duration, filter.step)
            .num_seconds()
            .max(1);
        values.push((step_secs as f64).into());
        let step = format!("make_interval(secs => ${})", values.len());

        if filter.monotonic {
            // Cumulative counter stored as raw values (OTLP path).
            //
            // OTLP exports send one data point per label-set (e.g. per
            // operation type), all at the same timestamp. Each data point
            // is a cumulative total for that label. The "grand total" is
            // the MAX across all label-set rows at each timestamp (RustFS
            // includes an unlabelled summary row that carries the total).
            //
            // We take MAX(value) per scrape timestamp first (collapses all
            // label-set rows into the single highest value = the total),
            // then bucket those per-scrape maxes with MAX again, then apply
            // LAG to compute the increase over the bucket interval.
            // Resets (counter restart) floor at 0 for that bucket.
            format!(
                "SELECT bucket, GREATEST(bucket_max - LAG(bucket_max) OVER (ORDER BY bucket), 0) AS avg_value \
                 FROM ( \
                   SELECT time_bucket({step}, time) AS bucket, \
                          MAX(scrape_max) AS bucket_max \
                   FROM ( \
                     SELECT time, MAX(value) AS scrape_max \
                     FROM service_metrics \
                     WHERE source_kind = $1 \
                       AND source_id = $2 \
                       AND name = $3 \
                       AND time >= $4 \
                       AND time <= $5{label_filter} \
                     GROUP BY time \
                   ) per_scrape \
                   GROUP BY bucket \
                   ORDER BY bucket ASC \
                 ) sub"
            )
        } else {
            format!(
                "SELECT time_bucket({step}, time) AS bucket, AVG(value) AS avg_value \
                 FROM service_metrics \
                 WHERE source_kind = $1 \
                   AND source_id = $2 \
                   AND name = $3 \
                   AND time >= $4 \
                   AND time <= $5{label_filter} \
                 GROUP BY bucket \
                 ORDER BY bucket ASC"
            )
        }
    } else {
        // Hourly continuous aggregate (≤ 90 days) or daily (> 90 days).
        // NOTE: data in the trailing `end_offset` (1 hour for hourly, 1 day
        // for daily) may not yet be refreshed into the view, so the right
        // edge of the result may be missing one bucket.
        let view = if range_duration <= ninety_days {
            "service_metrics_hourly"
        } else {
            "service_metrics_daily"
        };
        format!(
            "SELECT bucket, avg_value \
             FROM {view} \
             WHERE source_kind = $1 \
               AND source_id = $2 \
               AND name = $3 \
               AND bucket >= $4 \
               AND bucket <= $5{label_filter} \
             ORDER BY bucket ASC"
        )
    };

    pg_statement(sql, values)
}

/// Build the "latest value per metric" query. `names` must already be
/// validated; an empty slice means "every metric for this source".
fn build_latest_statement(source_kind: &SourceKind, source_id: i32, names: &[&str]) -> Statement {
    // $1 source_kind, $2 source_id, $3.. names
    let mut values: Vec<Value> = vec![source_kind.as_str().into(), source_id.into()];
    let name_filter = if names.is_empty() {
        String::new()
    } else {
        let list = placeholders(values.len() + 1, names.len());
        values.extend(names.iter().map(|n| Value::from(n.to_string())));
        format!("AND name IN ({list})")
    };

    // `DISTINCT ON (name)` keeps one row per metric name. Some metrics are
    // written as multiple label-series per scrape (e.g. Postgres emits
    // `pg.database_size_bytes` once per `datname` PLUS one instance-wide
    // aggregate). For the single stat-tile value we always want the
    // aggregate row, never an arbitrary per-label one. The aggregate is the
    // row with the FEWEST label keys: per-series rows add a dimension key
    // (e.g. `datname`, `replica_addr`) on top of the shared base labels
    // (`engine`, `environment`), while the aggregate carries only the base
    // labels. So order by label-key count ascending, then by recency.
    // (An empty `{}` is just the zero-key case and still wins.) Metrics with
    // a single series are unaffected.
    //
    // PERF: the query MUST be time-bounded. The label-key-count ordering
    // prevents the planner from satisfying `DISTINCT ON` with a backwards
    // index scan, so without a bound this degenerates into a full scan of
    // every chunk the source ever wrote (millions of rows at a 10-30s
    // scrape interval) with the jsonb subquery evaluated per row. Bounding
    // relative to `service_metrics_status.last_received_at` (O(1) row,
    // upserted on every write_batch) keeps chunk exclusion active while
    // still returning values for sources whose scraper is paused/stale.
    let sql = format!(
        "SELECT DISTINCT ON (name) name, value \
         FROM service_metrics \
         WHERE source_kind = $1 \
           AND source_id = $2 \
           AND time > COALESCE( \
                 (SELECT last_received_at FROM service_metrics_status \
                   WHERE source_kind = $1 AND source_id = $2), \
                 now()) - interval '{LATEST_WINDOW}' \
           {name_filter} \
         ORDER BY name, \
                  (SELECT count(*) FROM jsonb_object_keys(labels)) ASC, \
                  time DESC"
    );
    pg_statement(sql, values)
}

/// Build the "latest value per (metric, label value)" query. `label_key` and
/// `names` must already be validated and `names` must be non-empty.
fn build_latest_by_label_statement(
    source_kind: &SourceKind,
    source_id: i32,
    label_key: &str,
    names: &[&str],
) -> Statement {
    // $1 source_kind, $2 source_id, $3 label_key, $4.. names
    let mut values: Vec<Value> = vec![
        source_kind.as_str().into(),
        source_id.into(),
        label_key.to_string().into(),
    ];
    let list = placeholders(values.len() + 1, names.len());
    values.extend(names.iter().map(|n| Value::from(n.to_string())));

    // For each (name, label_value) keep the most-recent row. Only rows that
    // carry the label key are considered (`labels ? key`), which excludes
    // the unlabelled instance-wide aggregate. The `DISTINCT ON` key is
    // (name, label_value) so each metric gets one value per label value.
    //
    // PERF: time-bounded for the same reason as `query_latest` — the
    // `labels ? key` predicate and the label-value DISTINCT key defeat a
    // backwards index scan, so an unbounded query scans the source's full
    // retention history. See the comment there for the COALESCE fallback.
    let sql = format!(
        "SELECT DISTINCT ON (name, labels->>$3) \
                name, labels->>$3 AS label_value, value \
         FROM service_metrics \
         WHERE source_kind = $1 \
           AND source_id = $2 \
           AND time > COALESCE( \
                 (SELECT last_received_at FROM service_metrics_status \
                   WHERE source_kind = $1 AND source_id = $2), \
                 now()) - interval '{LATEST_WINDOW}' \
           AND name IN ({list}) \
           AND labels ? $3 \
         ORDER BY name, labels->>$3, time DESC"
    );
    pg_statement(sql, values)
}

impl TimescaleMetricsStore {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// The minimum label-key count across the recent rows of a metric, or
    /// `None` if the metric has no rows / on error.
    ///
    /// The instance-wide aggregate row is the one with the FEWEST label keys
    /// (per-series rows add a dimension key like `datname`/`replica_addr` on top
    /// of the shared base labels). `query_range` uses this to scope a chart to
    /// the aggregate series instead of blending every per-label row together.
    /// Returns `None` on error so the caller falls back to an unfiltered query
    /// rather than charting nothing. Bounded to the recent window via `LIMIT`.
    async fn min_label_key_count(
        &self,
        source_kind: &SourceKind,
        source_id: i32,
        name: &str,
    ) -> Option<i64> {
        if validate_metric_name(name).is_err() {
            return None;
        }
        // Look only at the most-recent ~64 rows: a single scrape writes all of a
        // metric's series at once, so the recent window contains every series.
        // Time-bounded so TimescaleDB chunk exclusion applies (see query_latest).
        let sql = format!(
            "SELECT min(k)::bigint AS min_keys FROM ( \
                 SELECT (SELECT count(*) FROM jsonb_object_keys(labels)) AS k \
                 FROM service_metrics \
                 WHERE source_kind = $1 AND source_id = $2 AND name = $3 \
                   AND time > COALESCE( \
                         (SELECT last_received_at FROM service_metrics_status \
                           WHERE source_kind = $1 AND source_id = $2), \
                         now()) - interval '{LATEST_WINDOW}' \
                 ORDER BY time DESC LIMIT 64 \
             ) recent"
        );
        let stmt = pg_statement(
            sql,
            vec![
                source_kind.as_str().into(),
                source_id.into(),
                name.to_string().into(),
            ],
        );
        match self.db.query_one(stmt).await {
            Ok(Some(row)) => row.try_get::<Option<i64>>("", "min_keys").ok().flatten(),
            _ => None,
        }
    }
}

/// Validate that a metric name contains only safe characters.
///
/// Allowed: ASCII alphanumeric, underscore `_`, dot `.`, hyphen `-`, colon `:`.
///
/// # SECURITY(metrics-security-1)
///
/// The store binds metric names as parameters, so this is no longer the
/// injection boundary; it remains the input contract for user-supplied metric
/// names (alert rules, OTLP ingest) and a defence-in-depth layer. Call it for
/// every metric name that originates from a user-supplied data source.
///
/// Returns `Err(metric_name)` when the name contains forbidden characters.
pub fn validate_metric_name(name: &str) -> Result<(), &str> {
    if name.is_empty() {
        return Err(name);
    }
    for ch in name.chars() {
        if !matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.' | '-' | ':') {
            return Err(name);
        }
    }
    Ok(())
}

#[async_trait]
impl MetricsStore for TimescaleMetricsStore {
    /// Bulk-inserts all gauge/counter points into `service_metrics`, chunked
    /// at [`BATCH_SIZE`] rows per statement. Each chunk is a single
    /// parameterised statement (see [`INSERT_METRICS_SQL`]).
    ///
    /// Points with NaN or infinite values, or names outside the allowlist,
    /// are skipped with a warning rather than aborting the entire batch.
    ///
    /// # Safety contract for `MetricKind::Counter`
    ///
    /// The store writes whatever `value` it receives without performing
    /// counter-delta computation. Callers **must** ensure that `value` is
    /// already a non-negative delta for Counter points. The scraper is
    /// responsible for computing `current − previous` before calling
    /// `write_batch`. A `debug_assert!` enforces this contract in
    /// development builds.
    async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError> {
        if points.is_empty() {
            return Ok(());
        }

        for chunk in points.chunks(BATCH_SIZE) {
            let Some(stmt) = build_insert_statement(chunk) else {
                continue;
            };
            self.db
                .execute(stmt)
                .await
                .map_err(MetricsError::DatabaseError)?;
        }

        // Maintain the per-source "last received" status row so the UI can show
        // a freshness timestamp with an O(1) lookup instead of MAX(time) over
        // the hypertable. One row per distinct source in this batch.
        if let Some(stmt) = build_status_statement(&points) {
            // Non-fatal — a failure here must not lose the already-written metrics.
            if let Err(e) = self.db.execute(stmt).await {
                warn!(error = %e, "Failed to update service_metrics_status (non-fatal)");
            }
        }

        Ok(())
    }

    /// Returns bucketed `(timestamp, avg_value)` series for the query range.
    ///
    /// Table selection uses `Duration` comparisons (not `num_hours()` integer
    /// truncation) to avoid boundary rounding surprises:
    /// - range ≤ 7 days  → raw `service_metrics` (always present; avoids the
    ///   1-hour cold-start window where hourly CA has no data)
    /// - range ≤ 90 days → `service_metrics_hourly` continuous aggregate
    /// - range > 90 days → `service_metrics_daily`  continuous aggregate
    ///
    /// **Known limitation:** continuous aggregates have a trailing gap equal to
    /// their `end_offset` (1 hour for hourly, 1 day for daily). Data in that
    /// window is in the raw table but not yet in the aggregate. Queries near
    /// the aggregate boundary may therefore appear sparse at the right edge.
    /// This is expected behaviour and not surfaced as an error.
    ///
    /// **Retention race:** `query_range()` may rarely encounter a
    /// `DatabaseError` if a TimescaleDB retention policy drops a chunk
    /// exactly mid-query (TimescaleDB < 2.10). The error propagates to the
    /// caller.
    ///
    /// # TODO(metrics): Issue 10 — wrap query in a single retry with a 50 ms
    /// delay to handle the chunk-drop-mid-query race in TimescaleDB < 2.10.
    async fn query_range(
        &self,
        filter: RangeQuery,
    ) -> Result<Vec<(DateTime<Utc>, f64)>, MetricsError> {
        if validate_metric_name(&filter.name).is_err() {
            warn!(
                metric_name = %filter.name,
                "query_range: metric name contains invalid characters; returning empty result"
            );
            return Ok(vec![]);
        }

        // Some metrics are written as multiple label-series per scrape (e.g.
        // Postgres emits `pg.cache_hit_ratio` / `pg.database_size_bytes` once
        // per `datname` PLUS one instance-wide aggregate). For a single chart
        // series we want the aggregate, never a blend of every per-db row (AVG
        // across databases is meaningless for a ratio, and double-counts a
        // size). The aggregate is the series with the FEWEST label keys; we
        // scope the query to rows whose key count equals that minimum, so
        // per-`datname` rows (which add one key) are excluded. Metrics with a
        // single series have a single key count, so the filter is a no-op for
        // them (per-replica lag, connection counts, etc.).
        //
        // The hourly/daily continuous aggregates carry `labels` in their GROUP
        // BY (m20260601_000009), so this same filter is valid on every range.
        let min_keys = self
            .min_label_key_count(&filter.source_kind, filter.source_id, &filter.name)
            .await;

        let rows = self
            .db
            .query_all(build_range_statement(&filter, min_keys))
            .await
            .map_err(MetricsError::DatabaseError)?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let bucket: DateTime<Utc> = row
                .try_get("", "bucket")
                .map_err(MetricsError::DatabaseError)?;
            let avg_value: f64 = row
                .try_get("", "avg_value")
                .map_err(MetricsError::DatabaseError)?;
            result.push((bucket, avg_value));
        }

        Ok(result)
    }

    /// Returns the most-recent value for each of the requested metric names.
    ///
    /// Uses `DISTINCT ON (name)` ordered by `(name, time DESC)` so only the
    /// latest row per name is returned. Returns only those names that have at
    /// least one row in the raw table; names that have never been written are
    /// absent from the result `HashMap` (not an error). Callers — particularly
    /// `AlertEvaluator` — must treat absence as "metric not yet available"
    /// rather than "threshold not breached".
    ///
    /// Names outside the [`validate_metric_name`] allowlist are excluded from
    /// the result (same semantics as "no data" — not an error, not a breach
    /// trigger).
    ///
    /// # TODO(metrics): Issue 3 — the composite index `(source_id, name, time DESC)`
    /// does not include `source_kind`, so PostgreSQL filters `source_kind`
    /// post-scan. If `source_id` values are globally unique across entity types
    /// this is harmless, but if two entity types share the same integer ID the
    /// index scan returns extra rows. Add `source_kind` to the index or enforce
    /// globally unique source IDs via a registry table.
    async fn query_latest(
        &self,
        filter: LatestQuery,
    ) -> Result<HashMap<String, f64>, MetricsError> {
        // Empty names = "return latest value for every metric tracked for this source".
        let names = valid_metric_names(&filter.names, "query_latest");
        if !filter.names.is_empty() && names.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = self
            .db
            .query_all(build_latest_statement(
                &filter.source_kind,
                filter.source_id,
                &names,
            ))
            .await
            .map_err(MetricsError::DatabaseError)?;

        let mut result = HashMap::with_capacity(rows.len());
        for row in rows {
            let name: String = row
                .try_get("", "name")
                .map_err(MetricsError::DatabaseError)?;
            let value: f64 = row
                .try_get("", "value")
                .map_err(MetricsError::DatabaseError)?;
            result.insert(name, value);
        }

        Ok(result)
    }

    async fn query_latest_by_label(
        &self,
        filter: LatestByLabelQuery,
    ) -> Result<Vec<LabelledMetric>, MetricsError> {
        // The label key comes from server-side handler constants today; it is
        // bound as a parameter, and validated with the metric-name allowlist
        // so a future caller cannot widen the contract unnoticed.
        if validate_metric_name(&filter.label_key).is_err() {
            warn!(
                label_key = %filter.label_key,
                "query_latest_by_label: label key contains invalid characters; returning empty"
            );
            return Ok(Vec::new());
        }

        let names = valid_metric_names(&filter.names, "query_latest_by_label");
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let rows = self
            .db
            .query_all(build_latest_by_label_statement(
                &filter.source_kind,
                filter.source_id,
                &filter.label_key,
                &names,
            ))
            .await
            .map_err(MetricsError::DatabaseError)?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row
                .try_get("", "name")
                .map_err(MetricsError::DatabaseError)?;
            let label_value: String = row
                .try_get("", "label_value")
                .map_err(MetricsError::DatabaseError)?;
            let value: f64 = row
                .try_get("", "value")
                .map_err(MetricsError::DatabaseError)?;
            result.push(LabelledMetric {
                label_value,
                name,
                value,
            });
        }

        Ok(result)
    }

    async fn latest_timestamp(
        &self,
        source_kind: SourceKind,
        source_id: i32,
    ) -> Result<Option<DateTime<Utc>>, MetricsError> {
        // O(1) primary-key lookup on the small status table — no hypertable
        // scan. The row is upserted on every write_batch.
        let stmt = pg_statement(
            "SELECT last_received_at \
             FROM service_metrics_status \
             WHERE source_kind = $1 AND source_id = $2",
            vec![source_kind.as_str().into(), source_id.into()],
        );

        let row = self
            .db
            .query_one(stmt)
            .await
            .map_err(MetricsError::DatabaseError)?;

        match row {
            Some(r) => Ok(Some(
                r.try_get::<DateTime<Utc>>("", "last_received_at")
                    .map_err(MetricsError::DatabaseError)?,
            )),
            None => Ok(None),
        }
    }

    /// Drops raw metric chunks older than `older_than` using TimescaleDB's
    /// `drop_chunks()` rather than a `DELETE` statement.
    ///
    /// **Why not DELETE?** A `DELETE WHERE time < X` on a hypertable acquires
    /// row-level locks, rewrites WAL, and updates every index (including the
    /// GIN index) for every matched row. TimescaleDB's `drop_chunks()` drops
    /// entire chunk files atomically at O(1) cost, matching what the built-in
    /// retention policy does. Using DELETE would compete with the retention
    /// policy background job via a lock convoy (row lock vs AccessExclusiveLock
    /// on the same chunk) and is far more expensive.
    ///
    /// Returns the number of chunks dropped (not rows — rows per chunk vary).
    async fn prune(&self, older_than: DateTime<Utc>) -> Result<u64, MetricsError> {
        let stmt = pg_statement(
            "SELECT drop_chunks('service_metrics', $1::timestamptz)",
            vec![older_than.into()],
        );

        let rows = self
            .db
            .query_all(stmt)
            .await
            .map_err(MetricsError::DatabaseError)?;

        Ok(rows.len() as u64)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MetricKind, SourceKind};
    use chrono::{Duration, Utc};

    fn make_gauge(name: &str, value: f64) -> MetricPoint {
        MetricPoint {
            time: Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 1,
            name: name.to_string(),
            value,
            kind: MetricKind::Gauge,
            engine: Some("postgres".to_string()),
            environment: Some("production".to_string()),
            node_id: None,
            labels: HashMap::new(),
        }
    }

    #[test]
    fn test_source_kind_as_str() {
        assert_eq!(SourceKind::Database.as_str(), "database");
        assert_eq!(SourceKind::Deployment.as_str(), "deployment");
        assert_eq!(SourceKind::Container.as_str(), "container");
        assert_eq!(SourceKind::Node.as_str(), "node");
    }

    #[test]
    fn test_metric_point_construction() {
        let p = make_gauge("pg.connections_active", 42.0);
        assert_eq!(p.name, "pg.connections_active");
        assert_eq!(p.value, 42.0);
        assert_eq!(p.source_id, 1);
        assert!(matches!(p.kind, MetricKind::Gauge));
        assert!(matches!(p.source_kind, SourceKind::Database));
    }

    #[test]
    fn test_range_query_construction() {
        let from = Utc::now() - Duration::hours(2);
        let to = Utc::now();
        let q = RangeQuery {
            source_kind: SourceKind::Database,
            source_id: 1,
            name: "pg.connections_active".to_string(),
            from,
            to,
            step: Duration::seconds(30),
            monotonic: false,
        };
        assert_eq!(q.step.num_seconds(), 30);
        assert!((q.to - q.from) <= chrono::Duration::days(7));
    }

    #[test]
    fn test_latest_query_construction() {
        let q = LatestQuery {
            source_kind: SourceKind::Node,
            source_id: 5,
            names: vec![
                "node.cpu_pct".to_string(),
                "node.mem_used_bytes".to_string(),
            ],
        };
        assert_eq!(q.names.len(), 2);
    }

    // ── Parameterised statements (SECURITY metrics-security-1) ──────────

    const HOSTILE: &str = "x'); DROP TABLE service_metrics; --";
    const HOSTILE_BACKSLASH: &str = "prod\\'; DELETE FROM service_metrics; --";

    fn values_debug(stmt: &Statement) -> String {
        format!("{:?}", stmt.values)
    }

    fn bound_value_count(stmt: &Statement) -> usize {
        stmt.values.as_ref().map(|v| v.0.len()).unwrap_or(0)
    }

    /// Highest `$N` placeholder in the SQL text.
    fn max_placeholder(sql: &str) -> usize {
        sql.match_indices('$')
            .filter_map(|(i, _)| {
                sql[i + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<usize>()
                    .ok()
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn insert_statement_binds_untrusted_strings_instead_of_splicing_them() {
        let mut p = make_gauge("pg.connections_active", 1.0);
        p.environment = Some(HOSTILE_BACKSLASH.to_string());
        p.engine = Some(HOSTILE.to_string());
        p.labels
            .insert(HOSTILE.to_string(), HOSTILE_BACKSLASH.to_string());

        let stmt = build_insert_statement(&[p]).expect("one valid point");
        assert_eq!(
            stmt.sql, INSERT_METRICS_SQL,
            "statement text must be constant"
        );
        assert!(!stmt.sql.contains("DROP") && !stmt.sql.contains("DELETE"));
        assert_eq!(bound_value_count(&stmt), 1);
        assert_eq!(max_placeholder(&stmt.sql), 1);
        let bound = values_debug(&stmt);
        assert!(
            bound.contains("DROP TABLE"),
            "hostile value travels as data"
        );
        assert!(
            bound.contains("DELETE FROM"),
            "hostile value travels as data"
        );
    }

    #[test]
    fn insert_statement_drops_invalid_names_and_non_finite_values() {
        assert!(build_insert_statement(&[make_gauge(HOSTILE, 1.0)]).is_none());
        assert!(build_insert_statement(&[make_gauge("ok.metric", f64::NAN)]).is_none());
        assert!(build_insert_statement(&[make_gauge("ok.metric", f64::INFINITY)]).is_none());
        assert!(build_insert_statement(&[]).is_none());

        let stmt = build_insert_statement(&[
            make_gauge("ok.metric", 2.5),
            make_gauge(HOSTILE, 9.0),
            make_gauge("ok.metric", f64::NEG_INFINITY),
        ])
        .expect("one valid point survives");
        let bound = values_debug(&stmt);
        assert!(bound.contains("ok.metric"));
        assert!(
            !bound.contains("DROP TABLE"),
            "invalid-name point is dropped"
        );
    }

    #[test]
    fn status_statement_has_one_row_per_source_with_latest_time() {
        let mut a = make_gauge("m.a", 1.0);
        let mut b = make_gauge("m.b", 1.0);
        let older = Utc::now() - Duration::minutes(5);
        a.time = older;
        b.time = older + Duration::minutes(1);
        let mut other = make_gauge("m.c", 1.0);
        other.source_id = 2;

        let stmt = build_status_statement(&[a, b.clone(), other]).expect("non-empty");
        assert_eq!(stmt.sql, UPSERT_STATUS_SQL);
        let Some(values) = stmt.values.as_ref() else {
            panic!("status statement must carry its rows as a bind value");
        };
        let Value::Json(Some(json)) = &values.0[0] else {
            panic!("status rows must be bound as JSON, got {:?}", values.0[0]);
        };
        let rows = json.as_array().expect("array of rows");
        assert_eq!(rows.len(), 2, "one row per (source_kind, source_id)");
        let src1 = rows
            .iter()
            .find(|r| r["source_id"] == 1)
            .expect("row for source 1");
        assert_eq!(
            src1["last_received_at"],
            b.time.to_rfc3339_opts(SecondsFormat::Micros, true)
        );
        assert!(build_status_statement(&[]).is_none());
    }

    #[test]
    fn range_statement_uses_placeholders_for_every_value() {
        let now = Utc::now();
        for (span, monotonic, min_keys) in [
            (Duration::hours(2), false, None),
            (Duration::hours(2), true, Some(2)),
            (Duration::days(30), false, Some(1)),
            (Duration::days(200), false, None),
        ] {
            let q = RangeQuery {
                source_kind: SourceKind::Database,
                source_id: 7,
                name: "pg.connections_active".to_string(),
                from: now - span,
                to: now,
                step: Duration::seconds(30),
                monotonic,
            };
            let stmt = build_range_statement(&q, min_keys);
            assert!(!stmt.sql.contains("pg.connections_active"));
            assert!(!stmt.sql.contains("'database'"));
            assert_eq!(
                max_placeholder(&stmt.sql),
                bound_value_count(&stmt),
                "every bound value is referenced and vice versa: {}",
                stmt.sql
            );
        }
    }

    #[test]
    fn latest_statements_bind_names_and_label_key() {
        let stmt = build_latest_statement(&SourceKind::Node, 3, &["a.b", "c.d"]);
        assert!(!stmt.sql.contains("a.b") && !stmt.sql.contains("c.d"));
        assert!(stmt.sql.contains("name IN ($3, $4)"));
        assert_eq!(bound_value_count(&stmt), 4);

        let all = build_latest_statement(&SourceKind::Node, 3, &[]);
        assert!(!all.sql.contains("name IN"));
        assert_eq!(bound_value_count(&all), 2);

        let by_label =
            build_latest_by_label_statement(&SourceKind::Database, 3, "datname", &["pg.size"]);
        assert!(!by_label.sql.contains("datname") && !by_label.sql.contains("pg.size"));
        assert_eq!(max_placeholder(&by_label.sql), bound_value_count(&by_label));
    }

    // ── validate_metric_name ──────────────────────────────────────────

    #[test]
    fn test_validate_metric_name_valid() {
        assert!(validate_metric_name("pg.connections_active").is_ok());
        assert!(validate_metric_name("redis.evicted_keys_total").is_ok());
        assert!(validate_metric_name("container.cpu_percent").is_ok());
        assert!(validate_metric_name("node.mem_used_bytes").is_ok());
        assert!(validate_metric_name("A-Z_0.9:metric").is_ok());
    }

    #[test]
    fn test_validate_metric_name_empty_rejected() {
        assert!(validate_metric_name("").is_err());
    }

    #[test]
    fn test_validate_metric_name_sql_injection_rejected() {
        // SECURITY(metrics-security-1): these must all be rejected.
        assert!(validate_metric_name("'; DROP TABLE service_metrics; --").is_err());
        assert!(validate_metric_name("metric' OR '1'='1").is_err());
        assert!(validate_metric_name("name with space").is_err());
        assert!(validate_metric_name("name\nnewline").is_err());
        assert!(validate_metric_name("name;semicolon").is_err());
    }

    #[test]
    fn test_validate_metric_name_allowed_special_chars() {
        // Dots, hyphens, underscores, colons are all valid.
        assert!(validate_metric_name("pg.cache_hit_ratio").is_ok());
        assert!(validate_metric_name("my-service:metric_v2").is_ok());
    }

    #[test]
    fn test_rfc3339_micros_format() {
        let ts = Utc::now();
        let s = ts.to_rfc3339_opts(SecondsFormat::Micros, true);
        // Must end with 'Z', not '+00:00'
        assert!(s.ends_with('Z'), "expected Z suffix, got: {s}");
        // Must not have nanosecond precision (>6 digits after decimal)
        let dot_pos = s.find('.').unwrap();
        let z_pos = s.rfind('Z').unwrap();
        let fractional_len = z_pos - dot_pos - 1;
        assert_eq!(
            fractional_len, 6,
            "expected 6 fractional digits, got {fractional_len}"
        );
    }

    // ── write_batch metric-name validation (SECURITY metrics-security-1) ───────
    //
    // These verify that `write_batch` applies the `validate_metric_name`
    // allowlist before interpolating the name into the metrics INSERT.
    //
    // `write_batch` executes up to two statements per call:
    //   1. the metrics INSERT — ONLY when at least one point survives validation
    //      (an all-dropped chunk hits `rows.is_empty()` → `continue`, no INSERT)
    //   2. the `service_metrics_status` freshness upsert — always runs for a
    //      non-empty input batch, and is name-independent (source_kind is an
    //      enum, source_id is i32), so it carries no injection risk.
    //
    // `Transaction` doesn't expose its SQL text, so we assert on the count of
    // statements the MockDatabase logged. The signal is the metrics INSERT:
    //   • all-invalid input  → 1 statement  (status upsert only, no INSERT)
    //   • one valid point     → 2 statements (metrics INSERT + status upsert)
    // The difference of exactly one INSERT proves the malicious name was
    // dropped before reaching SQL — a kept row would have produced an INSERT.

    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    /// Build a store over a MockDatabase that accepts up to `n` execute() calls.
    fn mock_store(n: usize) -> (TimescaleMetricsStore, Arc<DatabaseConnection>) {
        let exec_results: Vec<MockExecResult> = (0..n)
            .map(|_| MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })
            .collect();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results(exec_results)
                .into_connection(),
        );
        (TimescaleMetricsStore::new(db.clone()), db)
    }

    /// Number of statements the mock actually executed. Consumes the store so
    /// its `Arc<DatabaseConnection>` clone is dropped, leaving this `db` as the
    /// sole owner for `into_transaction_log()`.
    fn executed_count(store: TimescaleMetricsStore, db: Arc<DatabaseConnection>) -> usize {
        drop(store);
        Arc::try_unwrap(db)
            .expect("store dropped, so this is the only remaining ref")
            .into_transaction_log()
            .len()
    }

    #[tokio::test]
    async fn write_batch_skips_malicious_metric_name() {
        // A single point whose name is a SQL-injection payload must be dropped:
        // no surviving rows → NO metrics INSERT. Only the name-independent
        // status upsert runs (1 statement), never the metrics INSERT.
        let (store, db) = mock_store(2); // allow up to 2 so a stray INSERT wouldn't error out

        store
            .write_batch(vec![make_gauge("x'); DROP TABLE service_metrics; --", 1.0)])
            .await
            .expect("write_batch should succeed (the bad point is skipped, not an error)");

        assert_eq!(
            executed_count(store, db),
            1,
            "all-invalid batch must run ONLY the status upsert — no metrics INSERT \
             (the malicious name was dropped before SQL)"
        );
    }

    #[tokio::test]
    async fn write_batch_drops_only_the_malicious_point() {
        // Mixed batch: the valid point survives (metrics INSERT runs), the
        // malicious one is dropped. 2 statements = metrics INSERT + status
        // upsert — same as a fully-valid single-point batch, proving the bad
        // point neither blocked the write nor added a second INSERT.
        let (store, db) = mock_store(2);

        store
            .write_batch(vec![
                make_gauge("pg.connections", 5.0),
                make_gauge("evil'); DELETE FROM service_metrics WHERE '1'='1", 9.0),
            ])
            .await
            .expect("write_batch should succeed");

        assert_eq!(
            executed_count(store, db),
            2,
            "metrics INSERT (for the one valid point) + status upsert"
        );
    }

    #[tokio::test]
    async fn write_batch_inserts_valid_name() {
        // Sanity baseline: a single valid point → metrics INSERT + status
        // upsert = 2 statements.
        let (store, db) = mock_store(2);

        store
            .write_batch(vec![make_gauge("redis.connected_clients", 3.0)])
            .await
            .expect("write_batch should succeed");

        assert_eq!(executed_count(store, db), 2);
    }

    // ── Real-database round trip ─────────────────────────────────────────

    /// Hostile strings must round-trip as data through every read/write path
    /// against a real TimescaleDB schema, and the parameterised SQL must be
    /// accepted by the server (placeholder types, `jsonb_to_recordset`,
    /// `make_interval`, `drop_chunks`).
    #[tokio::test]
    async fn parameterised_queries_round_trip_hostile_values_on_real_db() {
        use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};

        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping metrics store database test: {error}");
                return;
            }
            Err(error) => panic!("metrics store test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let store = TimescaleMetricsStore::new(db.clone());
        let source_id = 900_001;

        let now = Utc::now();
        let mut aggregate = make_gauge("pg.database_size_bytes", 100.0);
        aggregate.source_id = source_id;
        aggregate.time = now - Duration::seconds(30);
        aggregate.environment = Some(HOSTILE_BACKSLASH.to_string());
        aggregate.engine = Some(HOSTILE.to_string());
        let mut per_db = make_gauge("pg.database_size_bytes", 40.0);
        per_db.source_id = source_id;
        per_db.time = aggregate.time;
        per_db
            .labels
            .insert("datname".to_string(), HOSTILE.to_string());

        store
            .write_batch(vec![aggregate.clone(), per_db, make_gauge(HOSTILE, 1.0)])
            .await
            .expect("write_batch");

        // Stored verbatim (read back with a parameterised query).
        let row = db
            .query_one(pg_statement(
                "SELECT environment, engine FROM service_metrics \
                 WHERE source_id = $1 AND labels = '{}'::jsonb",
                vec![source_id.into()],
            ))
            .await
            .expect("select")
            .expect("aggregate row");
        let environment: Option<String> = row.try_get("", "environment").expect("environment");
        let engine: Option<String> = row.try_get("", "engine").expect("engine");
        assert_eq!(environment.as_deref(), Some(HOSTILE_BACKSLASH));
        assert_eq!(engine.as_deref(), Some(HOSTILE));

        let latest = store
            .query_latest(LatestQuery {
                source_kind: SourceKind::Database,
                source_id,
                names: vec!["pg.database_size_bytes".to_string(), HOSTILE.to_string()],
            })
            .await
            .expect("query_latest");
        assert_eq!(latest.get("pg.database_size_bytes"), Some(&100.0));
        assert_eq!(latest.len(), 1, "invalid name is excluded, not executed");

        let by_label = store
            .query_latest_by_label(LatestByLabelQuery {
                source_kind: SourceKind::Database,
                source_id,
                names: vec!["pg.database_size_bytes".to_string()],
                label_key: "datname".to_string(),
            })
            .await
            .expect("query_latest_by_label");
        assert_eq!(by_label.len(), 1);
        assert_eq!(by_label[0].label_value, HOSTILE);
        assert_eq!(by_label[0].value, 40.0);

        let series = store
            .query_range(RangeQuery {
                source_kind: SourceKind::Database,
                source_id,
                name: "pg.database_size_bytes".to_string(),
                from: now - Duration::hours(1),
                to: now,
                step: Duration::seconds(60),
                monotonic: false,
            })
            .await
            .expect("query_range");
        assert_eq!(
            series.len(),
            1,
            "one bucket, scoped to the aggregate series"
        );
        assert_eq!(series[0].1, 100.0);

        // Long ranges hit the continuous aggregates; they must parse and run.
        for days in [30, 200] {
            store
                .query_range(RangeQuery {
                    source_kind: SourceKind::Database,
                    source_id,
                    name: "pg.database_size_bytes".to_string(),
                    from: now - Duration::days(days),
                    to: now,
                    step: Duration::hours(1),
                    monotonic: false,
                })
                .await
                .expect("aggregate query_range");
        }

        let ts = store
            .latest_timestamp(SourceKind::Database, source_id)
            .await
            .expect("latest_timestamp")
            .expect("status row written");
        assert_eq!(ts.timestamp_micros(), aggregate.time.timestamp_micros());

        store
            .prune(now - Duration::days(3650))
            .await
            .expect("prune runs with a bound timestamp");
    }
}
