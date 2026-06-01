use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

use crate::error::MetricsError;
use crate::store::{
    LabelledMetric, LatestByLabelQuery, LatestQuery, MetricKind, MetricPoint, MetricsStore,
    RangeQuery, SourceKind,
};

/// Maximum rows per INSERT statement. Larger batches produce multi-MB query
/// strings that stress PostgreSQL's parser and cause all-or-nothing failures.
/// At ~300 bytes/row, 500 rows ≈ 150 KB — well inside safe limits.
const BATCH_SIZE: usize = 500;

/// TimescaleDB-backed implementation of [`MetricsStore`].
///
/// Writes are chunked into batches of at most [`BATCH_SIZE`] rows using
/// multi-row `VALUES` statements. Reads select the correct table (raw /
/// hourly / daily) based on the query range so TimescaleDB chunk exclusion
/// is always active.
pub struct TimescaleMetricsStore {
    db: Arc<DatabaseConnection>,
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
        source_kind: &str,
        source_id: i32,
        name: &str,
    ) -> Option<i64> {
        if validate_metric_name(name).is_err() {
            return None;
        }
        // Look only at the most-recent ~64 rows: a single scrape writes all of a
        // metric's series at once, so the recent window contains every series.
        let sql = format!(
            "SELECT min(k)::bigint AS min_keys FROM ( \
                 SELECT (SELECT count(*) FROM jsonb_object_keys(labels)) AS k \
                 FROM service_metrics \
                 WHERE source_kind = '{sk}' AND source_id = {sid} AND name = '{nm}' \
                 ORDER BY time DESC LIMIT 64 \
             ) recent",
            sk = escape_sql_string(source_kind),
            sid = source_id,
            nm = escape_sql_string(name),
        );
        match self
            .db
            .query_one(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
            .await
        {
            Ok(Some(row)) => row.try_get::<Option<i64>>("", "min_keys").ok().flatten(),
            _ => None,
        }
    }
}

/// Escape a string for safe embedding in a single-quoted SQL literal.
///
/// Only single-quote doubling is applied. This is safe when PostgreSQL
/// `standard_conforming_strings = on` (the default since PG 9.1), because
/// backslash has no special meaning in that mode.
///
/// # TODO(metrics): Issue 11 — replace string interpolation entirely with
/// dynamic `$N` bind parameters via the sqlx `PgArguments` builder or a
/// `COPY … FROM STDIN` path. String interpolation is technical debt for a
/// user-controlled surface (OTLP metric names come from user applications).
#[inline]
fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

/// Validate that a metric name contains only safe characters.
///
/// Allowed: ASCII alphanumeric, underscore `_`, dot `.`, hyphen `-`, colon `:`.
///
/// # SECURITY(metrics-security-1): metric_name SQL injection
///
/// Metric names from `monitoring_alert_rules` (user-controlled) are
/// interpolated into SQL via `escape_sql_string`.  The allowlist provides
/// defence-in-depth: even if `escape_sql_string` were bypassed, a metric name
/// containing `;`, `'`, `-`, or whitespace would be rejected before it reaches
/// the query builder.  This function must be called for every metric name that
/// originates from a user-supplied data source (alert rules, OTLP attribute
/// keys).
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

/// Convert an `f64` to a SQL-safe string. Returns `None` if the value is
/// NaN or infinite — callers skip such points and log a warning.
#[inline]
fn f64_to_sql(v: f64) -> Option<String> {
    if v.is_nan() || v.is_infinite() {
        None
    } else {
        Some(format!("{}", v))
    }
}

#[async_trait]
impl MetricsStore for TimescaleMetricsStore {
    /// Bulk-inserts all gauge/counter points into `service_metrics` using
    /// multi-row `VALUES` statements, chunked at [`BATCH_SIZE`] rows per
    /// statement. `ON CONFLICT DO NOTHING` makes writes idempotent.
    ///
    /// Points with NaN or infinite values are skipped with a warning rather
    /// than aborting the entire batch.
    ///
    /// # Safety contract for `MetricKind::Counter`
    ///
    /// The store writes whatever `value` it receives without performing
    /// counter-delta computation. Callers **must** ensure that `value` is
    /// already a non-negative delta for Counter points. The scraper is
    /// responsible for computing `current − previous` before calling
    /// `write_batch`. A `debug_assert!` below enforces this contract in
    /// development builds.
    ///
    /// # TODO(metrics): Issue 11 — migrate to `COPY … FROM STDIN` or
    /// prepared-statement bind parameters to eliminate string interpolation.
    async fn write_batch(&self, points: Vec<MetricPoint>) -> Result<(), MetricsError> {
        if points.is_empty() {
            return Ok(());
        }

        for chunk in points.chunks(BATCH_SIZE) {
            let mut rows: Vec<String> = Vec::with_capacity(chunk.len());

            for p in chunk {
                // SECURITY(metrics-security-1): validate the metric name before
                // it is interpolated into SQL below. Metric names on the OTLP
                // `si_` ingest path come straight off the wire (untrusted), and
                // the read path (`query_*`) already rejects names outside the
                // allowlist. Applying the same gate here keeps the write path
                // from being the weaker link: a name with SQL metacharacters is
                // dropped with a warning rather than escaped-and-stored.
                if validate_metric_name(&p.name).is_err() {
                    warn!(
                        metric = %p.name,
                        source_id = p.source_id,
                        "Skipping metric point: name contains characters outside the \
                         [a-zA-Z0-9_.:-] allowlist (possible injection attempt)"
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

                let value_sql = match f64_to_sql(p.value) {
                    Some(v) => v,
                    None => {
                        warn!(
                            metric = %p.name,
                            value = %p.value,
                            "Skipping metric point with non-finite value"
                        );
                        continue;
                    }
                };

                let labels_json = serde_json::to_string(&p.labels)
                    .map_err(|_| MetricsError::SerializationError)?;

                // Use microsecond precision with UTC 'Z' suffix — PostgreSQL's
                // TIMESTAMPTZ only stores microsecond resolution, and some builds
                // reject nanosecond strings in locale-dependent casts.
                let time_str = p.time.to_rfc3339_opts(SecondsFormat::Micros, true);

                let source_kind = escape_sql_string(p.source_kind.as_str());
                let name = escape_sql_string(&p.name);
                let engine = p
                    .engine
                    .as_deref()
                    .map(|s| format!("'{}'", escape_sql_string(s)))
                    .unwrap_or_else(|| "NULL".to_string());
                let environment = p
                    .environment
                    .as_deref()
                    .map(|s| format!("'{}'", escape_sql_string(s)))
                    .unwrap_or_else(|| "NULL".to_string());
                let node_id = p
                    .node_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "NULL".to_string());
                let labels_escaped = escape_sql_string(&labels_json);

                rows.push(format!(
                    "('{time}', '{source_kind}', {source_id}, '{name}', {value}, {engine}, {environment}, {node_id}, '{labels}'::jsonb)",
                    time = time_str,
                    source_kind = source_kind,
                    source_id = p.source_id,
                    name = name,
                    value = value_sql,
                    engine = engine,
                    environment = environment,
                    node_id = node_id,
                    labels = labels_escaped,
                ));
            }

            if rows.is_empty() {
                continue;
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
            let sql = format!(
                "INSERT INTO service_metrics \
                 (time, source_kind, source_id, name, value, engine, environment, node_id, labels) \
                 VALUES {} ON CONFLICT DO NOTHING",
                rows.join(", ")
            );

            self.db
                .execute(Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    sql,
                ))
                .await
                .map_err(MetricsError::DatabaseError)?;
        }

        // Maintain the per-source "last received" status row so the UI can show
        // a freshness timestamp with an O(1) lookup instead of MAX(time) over
        // the hypertable. One upsert per distinct source in this batch.
        let mut latest_by_source: HashMap<(String, i32), DateTime<Utc>> = HashMap::new();
        for p in &points {
            let key = (p.source_kind.as_str().to_string(), p.source_id);
            latest_by_source
                .entry(key)
                .and_modify(|t| {
                    if p.time > *t {
                        *t = p.time;
                    }
                })
                .or_insert(p.time);
        }
        if !latest_by_source.is_empty() {
            let values: Vec<String> = latest_by_source
                .iter()
                .map(|((sk, sid), t)| {
                    format!(
                        "('{}', {}, '{}')",
                        escape_sql_string(sk),
                        sid,
                        t.to_rfc3339_opts(SecondsFormat::Micros, true)
                    )
                })
                .collect();
            let status_sql = format!(
                "INSERT INTO service_metrics_status (source_kind, source_id, last_received_at) \
                 VALUES {} \
                 ON CONFLICT (source_kind, source_id) DO UPDATE \
                 SET last_received_at = GREATEST(service_metrics_status.last_received_at, EXCLUDED.last_received_at)",
                values.join(", ")
            );
            // Non-fatal — a failure here must not lose the already-written metrics.
            if let Err(e) = self
                .db
                .execute(Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    status_sql,
                ))
                .await
            {
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
        // SECURITY(metrics-security-1): validate metric name before SQL interpolation.
        if validate_metric_name(&filter.name).is_err() {
            warn!(
                metric_name = %filter.name,
                "query_range: metric name contains invalid characters; returning empty result"
            );
            return Ok(vec![]);
        }

        let range_duration = filter.to - filter.from;

        // Use Duration constants to avoid num_hours() integer truncation.
        let seven_days = chrono::Duration::days(7);
        let ninety_days = chrono::Duration::days(90);

        let from_str = filter.from.to_rfc3339_opts(SecondsFormat::Micros, true);
        let to_str = filter.to.to_rfc3339_opts(SecondsFormat::Micros, true);

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
            .min_label_key_count(filter.source_kind.as_str(), filter.source_id, &filter.name)
            .await;
        let raw_label_filter = match min_keys {
            Some(k) => format!(" AND (SELECT count(*) FROM jsonb_object_keys(labels)) = {k}"),
            None => String::new(),
        };
        let raw_label_filter = raw_label_filter.as_str();

        let sql = if range_duration <= seven_days {
            // Raw table — use time_bucket with the requested step.
            let step_secs = filter.step.num_seconds().max(1);
            let sk = escape_sql_string(filter.source_kind.as_str());
            let sid = filter.source_id;
            let nm = escape_sql_string(&filter.name);

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
                       SELECT time_bucket('{step_secs} seconds', time) AS bucket, \
                              MAX(scrape_max) AS bucket_max \
                       FROM ( \
                         SELECT time, MAX(value) AS scrape_max \
                         FROM service_metrics \
                         WHERE source_kind = '{sk}' \
                           AND source_id = {sid} \
                           AND name = '{nm}' \
                           AND time >= '{from}' \
                           AND time <= '{to}'{label_filter} \
                         GROUP BY time \
                       ) per_scrape \
                       GROUP BY bucket \
                       ORDER BY bucket ASC \
                     ) sub",
                    step_secs = step_secs,
                    sk = sk, sid = sid, nm = nm,
                    from = from_str, to = to_str,
                    label_filter = raw_label_filter,
                )
            } else {
                format!(
                    "SELECT time_bucket('{step_secs} seconds', time) AS bucket, AVG(value) AS avg_value \
                     FROM service_metrics \
                     WHERE source_kind = '{sk}' \
                       AND source_id = {sid} \
                       AND name = '{nm}' \
                       AND time >= '{from}' \
                       AND time <= '{to}'{label_filter} \
                     GROUP BY bucket \
                     ORDER BY bucket ASC",
                    step_secs = step_secs,
                    sk = sk, sid = sid, nm = nm,
                    from = from_str, to = to_str,
                    label_filter = raw_label_filter,
                )
            }
        } else if range_duration <= ninety_days {
            // Hourly continuous aggregate.
            // NOTE: data in the last 1 hour may not yet be refreshed into this
            // view (end_offset = INTERVAL '1 hour'). The trailing edge of the
            // result may therefore be missing one bucket.
            format!(
                "SELECT bucket, avg_value \
                 FROM service_metrics_hourly \
                 WHERE source_kind = '{source_kind}' \
                   AND source_id = {source_id} \
                   AND name = '{name}' \
                   AND bucket >= '{from}' \
                   AND bucket <= '{to}'{label_filter} \
                 ORDER BY bucket ASC",
                source_kind = escape_sql_string(filter.source_kind.as_str()),
                source_id = filter.source_id,
                name = escape_sql_string(&filter.name),
                from = from_str,
                to = to_str,
                label_filter = raw_label_filter,
            )
        } else {
            // Daily continuous aggregate.
            // NOTE: data in the last 1 day may not yet be refreshed into this
            // view (end_offset = INTERVAL '1 day').
            format!(
                "SELECT bucket, avg_value \
                 FROM service_metrics_daily \
                 WHERE source_kind = '{source_kind}' \
                   AND source_id = {source_id} \
                   AND name = '{name}' \
                   AND bucket >= '{from}' \
                   AND bucket <= '{to}'{label_filter} \
                 ORDER BY bucket ASC",
                source_kind = escape_sql_string(filter.source_kind.as_str()),
                source_id = filter.source_id,
                name = escape_sql_string(&filter.name),
                from = from_str,
                to = to_str,
                label_filter = raw_label_filter,
            )
        };

        let rows = self
            .db
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
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
    /// # SECURITY(metrics-security-1): metric name validation
    ///
    /// Metric names from alert rules (user-controlled) are embedded in SQL via
    /// string interpolation.  Each name is validated against the
    /// `[a-zA-Z0-9_.:−]` allowlist before being included in the query.
    /// Invalid names are silently excluded from the result (same semantics as
    /// "no data" — not an error, not a breach trigger).
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

        // Build the name filter clause.
        // Empty names = no filter (return all metrics for this source).
        // SECURITY(metrics-security-1): validate names to prevent injection.
        let name_filter = if filter.names.is_empty() {
            String::new() // no additional filter
        } else {
            let valid_names: Vec<&str> = filter
                .names
                .iter()
                .filter_map(|n| match validate_metric_name(n) {
                    Ok(()) => Some(n.as_str()),
                    Err(_) => {
                        warn!(
                            metric_name = %n,
                            "query_latest: metric name contains invalid characters; \
                             excluding from query (possible injection attempt)"
                        );
                        None
                    }
                })
                .collect();

            if valid_names.is_empty() {
                return Ok(HashMap::new());
            }

            let names_literal = valid_names
                .iter()
                .map(|n| format!("'{}'", escape_sql_string(n)))
                .collect::<Vec<_>>()
                .join(", ");

            format!("AND name = ANY(ARRAY[{}])", names_literal)
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
        let sql = format!(
            "SELECT DISTINCT ON (name) name, value \
             FROM service_metrics \
             WHERE source_kind = '{source_kind}' \
               AND source_id = {source_id} \
               {name_filter} \
             ORDER BY name, \
                      (SELECT count(*) FROM jsonb_object_keys(labels)) ASC, \
                      time DESC",
            source_kind = escape_sql_string(filter.source_kind.as_str()),
            source_id = filter.source_id,
            name_filter = name_filter,
        );

        let rows = self
            .db
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
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
        // SECURITY(metrics-security-1): the label key is interpolated into SQL.
        // It comes from server-side handler constants today, but validate it
        // with the same allowlist as metric names so a future caller can't
        // inject. Reject anything outside `[a-zA-Z0-9_.:-]`.
        if validate_metric_name(&filter.label_key).is_err() {
            warn!(
                label_key = %filter.label_key,
                "query_latest_by_label: label key contains invalid characters; returning empty"
            );
            return Ok(Vec::new());
        }

        // Validate metric names (same allowlist), dropping any invalid ones.
        let valid_names: Vec<&str> = filter
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
        if valid_names.is_empty() {
            return Ok(Vec::new());
        }

        let names_literal = valid_names
            .iter()
            .map(|n| format!("'{}'", escape_sql_string(n)))
            .collect::<Vec<_>>()
            .join(", ");

        let label_key = escape_sql_string(&filter.label_key);

        // For each (name, label_value) keep the most-recent row. Only rows that
        // carry the label key are considered (`labels ? key`), which excludes
        // the unlabelled instance-wide aggregate. The `DISTINCT ON` key is
        // (name, label_value) so each metric gets one value per label value.
        let sql = format!(
            "SELECT DISTINCT ON (name, labels->>'{label_key}') \
                    name, labels->>'{label_key}' AS label_value, value \
             FROM service_metrics \
             WHERE source_kind = '{source_kind}' \
               AND source_id = {source_id} \
               AND name = ANY(ARRAY[{names}]) \
               AND labels ? '{label_key}' \
             ORDER BY name, labels->>'{label_key}', time DESC",
            label_key = label_key,
            source_kind = escape_sql_string(filter.source_kind.as_str()),
            source_id = filter.source_id,
            names = names_literal,
        );

        let rows = self
            .db
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
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
        let sql = format!(
            "SELECT last_received_at \
             FROM service_metrics_status \
             WHERE source_kind = '{source_kind}' AND source_id = {source_id}",
            source_kind = escape_sql_string(source_kind.as_str()),
            source_id = source_id,
        );

        let row = self
            .db
            .query_one(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
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
        let older_than_str = older_than.to_rfc3339_opts(SecondsFormat::Micros, true);

        let sql = format!(
            "SELECT drop_chunks('service_metrics', '{}'::TIMESTAMPTZ)",
            older_than_str
        );

        let rows = self
            .db
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
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

    #[test]
    fn test_f64_to_sql_rejects_non_finite() {
        assert!(f64_to_sql(f64::NAN).is_none());
        assert!(f64_to_sql(f64::INFINITY).is_none());
        assert!(f64_to_sql(f64::NEG_INFINITY).is_none());
    }

    #[test]
    fn test_f64_to_sql_accepts_finite() {
        assert_eq!(f64_to_sql(0.0).unwrap(), "0");
        assert_eq!(f64_to_sql(1.5).unwrap(), "1.5");
        assert_eq!(f64_to_sql(-42.0).unwrap(), "-42");
    }

    #[test]
    fn test_escape_sql_string_quotes() {
        assert_eq!(escape_sql_string("it's"), "it''s");
        assert_eq!(escape_sql_string("no quotes"), "no quotes");
        assert_eq!(escape_sql_string("a''b"), "a''''b");
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
}
