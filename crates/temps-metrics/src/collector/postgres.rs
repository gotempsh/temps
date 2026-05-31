//! PostgreSQL metric collector.
//!
//! Connects to a Postgres instance using `tokio-postgres` with a 5-second
//! timeout (configurable via [`CollectorConfig::timeout`]) and runs several
//! read-only queries:
//!
//! 1. `pg_stat_activity` — active/idle connection counts, long-running and
//!    blocked query counts
//! 2. `pg_stat_database` — cache-hit ratio, commits, rollbacks, deadlocks,
//!    temp file usage, tuple DML rates, tuple fetch vs. return ratio
//! 3. `pg_stat_replication` — per-replica write and replay lag (seconds)
//! 4. `pg_stat_checkpointer` / `pg_stat_bgwriter` — checkpoint rates
//! 5. `pg_stat_wal` (PG 15+) — WAL bytes, records, full-page images, buffer
//!    full events; gracefully skipped on older versions
//! 6. `pg_stat_user_tables` aggregate — total live/dead tuples across all
//!    user tables (bloat indicator)
//! 7. `pg_locks` — count of waiting (blocked) lock requests
//! 8. `pg_database_size()` — size of the current database in bytes
//!
//! All errors are logged as warnings and result in an empty metric batch
//! being returned so the scraper loop is never blocked by a slow or
//! unreachable Postgres instance.

use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use tokio_postgres::NoTls;
use tracing::{debug, warn};

use super::{Collector, CollectorConfig};
use crate::error::MetricsError;
use crate::store::{MetricKind, MetricPoint};

/// Postgres metric collector.
///
/// Stateless — a fresh TCP connection is opened on every [`collect`] call.
/// This keeps the scraper's resource footprint low and avoids stale
/// connection state when a Postgres instance is restarted.
pub struct PostgresCollector;

impl PostgresCollector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PostgresCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for PostgresCollector {
    fn engine(&self) -> &'static str {
        "postgres"
    }

    async fn collect(&self, config: &CollectorConfig) -> Result<Vec<MetricPoint>, MetricsError> {
        let source_id = config.source_id;
        let conn_str = config.connection_string.clone();
        let timeout = config.timeout;

        debug!(
            source_id,
            engine = "postgres",
            "starting postgres metric collection"
        );

        // Open connection with timeout.
        let connect_result =
            tokio::time::timeout(timeout, tokio_postgres::connect(&conn_str, NoTls)).await;

        // SECURITY(metrics-security-3): never log the connection string — it
        // contains the database password.  On failure, log only source_id and
        // a sanitised error (tokio-postgres Error::Connect may embed the DSN
        // in its Display impl on some versions; we use a generic "connection
        // failed" message to avoid accidental credential exposure).
        let (client, connection) = match connect_result {
            Err(_elapsed) => {
                warn!(
                    source_id,
                    engine = "postgres",
                    timeout_secs = timeout.as_secs(),
                    "postgres connection timed out; skipping scrape"
                );
                return Ok(vec![]);
            }
            Ok(Err(_e)) => {
                // Deliberately not logging `_e` because tokio-postgres may
                // include the connection string (with credentials) in the
                // error Display on certain error variants.
                warn!(
                    source_id,
                    engine = "postgres",
                    "postgres connection failed; skipping scrape \
                     (connection error details withheld to prevent credential exposure)"
                );
                return Ok(vec![]);
            }
            Ok(Ok(pair)) => pair,
        };

        // Run metric queries and drive the connection concurrently using
        // tokio::select! so that both futures are cancelled together when the
        // timeout fires.  Using tokio::spawn for the connection driver would
        // leave a zombie background task holding an OS socket if the timeout
        // fires while queries are still in-flight.
        let collect_result = tokio::time::timeout(timeout, async {
            tokio::select! {
                // Connection driver completed (error or EOF from server side).
                conn_result = connection => {
                    if let Err(_e) = conn_result {
                        // Intentionally using %_e rather than %e — if needed for
                        // debugging, enable RUST_LOG=debug and check driver errors.
                        // Connection driver errors do not contain credentials.
                        warn!(
                            source_id,
                            engine = "postgres",
                            "postgres connection driver error during metric collection"
                        );
                    }
                    // If the driver exits, queries cannot proceed.
                    Err(tokio_postgres::Error::__private_api_timeout())
                }
                // Metric queries completed (success or query-level error).
                result = collect_metrics(&client, config) => result,
            }
        })
        .await;

        match collect_result {
            Err(_elapsed) => {
                warn!(
                    source_id,
                    engine = "postgres",
                    timeout_secs = timeout.as_secs(),
                    "postgres metric queries timed out; returning empty batch"
                );
                Ok(vec![])
            }
            Ok(Err(e)) => {
                // Query-level errors (e.g. permission denied on pg_stat_*) are
                // safe to log — they do not contain connection credentials.
                let detail = e
                    .as_db_error()
                    .map(|d| format!("{}: {}", d.severity(), d.message()))
                    .unwrap_or_else(|| e.to_string());
                warn!(
                    source_id,
                    engine = "postgres",
                    error = %detail,
                    "postgres metric query failed; returning empty batch"
                );
                Ok(vec![])
            }
            Ok(Ok(points)) => {
                debug!(
                    source_id,
                    engine = "postgres",
                    point_count = points.len(),
                    "postgres metric collection complete"
                );
                Ok(points)
            }
        }
    }
}

/// Run all pg_stat_* queries inside a single read-only transaction and
/// assemble the resulting [`MetricPoint`]s.
async fn collect_metrics(
    client: &tokio_postgres::Client,
    config: &CollectorConfig,
) -> Result<Vec<MetricPoint>, tokio_postgres::Error> {
    let mut points: Vec<MetricPoint> = Vec::with_capacity(32);
    let now = Utc::now();
    let source_id = config.source_id;
    let source_kind = config.source_kind.clone();

    // Build the common label set that every metric in this scrape shares.
    let mut base_labels: HashMap<String, String> = HashMap::new();
    base_labels.insert("engine".into(), "postgres".into());
    if let Some(env) = &config.environment {
        base_labels.insert("environment".into(), env.clone());
    }

    // Helper closure: create a metric point with the shared fields pre-filled.
    let make_point = |name: &str,
                      value: f64,
                      kind: MetricKind,
                      extra_labels: HashMap<String, String>|
     -> MetricPoint {
        let mut labels = base_labels.clone();
        labels.extend(extra_labels);
        MetricPoint {
            time: now,
            source_kind: source_kind.clone(),
            source_id,
            name: name.to_owned(),
            value,
            kind,
            engine: Some("postgres".into()),
            environment: config.environment.clone(),
            node_id: config.node_id,
            labels,
        }
    };

    // -------------------------------------------------------------------------
    // 1. pg_stat_activity — connection counts, long-running and blocked queries
    // -------------------------------------------------------------------------
    {
        let rows = client
            .query(
                "SELECT state, count(*)::bigint AS cnt \
                 FROM pg_stat_activity \
                 WHERE pid <> pg_backend_pid() \
                 GROUP BY state",
                &[],
            )
            .await?;

        let mut active: i64 = 0;
        let mut idle: i64 = 0;
        let mut idle_in_txn: i64 = 0;
        let mut other: i64 = 0;

        for row in &rows {
            // `state` can be NULL for walsender / autovacuum processes.
            let state: Option<&str> = row.get(0);
            let cnt: i64 = row.get(1);
            match state {
                Some("active") => active += cnt,
                Some("idle") => idle += cnt,
                Some("idle in transaction") => idle_in_txn += cnt,
                _ => other += cnt,
            }
        }

        points.push(make_point(
            "pg.connections_active",
            active as f64,
            MetricKind::Gauge,
            HashMap::new(),
        ));
        points.push(make_point(
            "pg.connections_idle",
            idle as f64,
            MetricKind::Gauge,
            HashMap::new(),
        ));
        points.push(make_point(
            "pg.connections_idle_in_transaction",
            idle_in_txn as f64,
            MetricKind::Gauge,
            HashMap::new(),
        ));
        points.push(make_point(
            "pg.connections_other",
            other as f64,
            MetricKind::Gauge,
            HashMap::new(),
        ));

        // Long-running queries (>30 s active): spikes here indicate missing
        // indexes, lock waits, or runaway queries.
        let long_running_rows = client
            .query(
                "SELECT count(*)::bigint \
                 FROM pg_stat_activity \
                 WHERE pid <> pg_backend_pid() \
                   AND state = 'active' \
                   AND query_start < NOW() - INTERVAL '30 seconds'",
                &[],
            )
            .await?;
        if let Some(row) = long_running_rows.first() {
            let cnt: i64 = row.get(0);
            points.push(make_point(
                "pg.queries_long_running",
                cnt as f64,
                MetricKind::Gauge,
                HashMap::new(),
            ));
        }

        // Blocked queries: backends waiting on a lock.  Any non-zero value
        // warrants investigation of the lock holder.
        let blocked_rows = client
            .query(
                "SELECT count(*)::bigint \
                 FROM pg_stat_activity \
                 WHERE pid <> pg_backend_pid() \
                   AND wait_event_type = 'Lock'",
                &[],
            )
            .await?;
        if let Some(row) = blocked_rows.first() {
            let cnt: i64 = row.get(0);
            points.push(make_point(
                "pg.queries_blocked",
                cnt as f64,
                MetricKind::Gauge,
                HashMap::new(),
            ));
        }
    }

    // -------------------------------------------------------------------------
    // 2. pg_stat_database — cache hit ratio, commits, rollbacks, deadlocks,
    //    temp file usage, tuple DML counters, tuple fetch/return ratio.
    //
    //    Emitted once per database with a `datname` label so the UI can filter
    //    or aggregate across all databases on the instance.  Internal Postgres
    //    databases (template0, template1) are excluded.
    // -------------------------------------------------------------------------
    {
        let rows = client
            .query(
                "SELECT datname, blks_hit, blks_read, \
                        xact_commit, xact_rollback, deadlocks, \
                        temp_files, temp_bytes, \
                        tup_inserted, tup_updated, tup_deleted, \
                        tup_returned, tup_fetched \
                 FROM pg_stat_database \
                 WHERE datname NOT IN ('template0', 'template1') \
                   AND datname IS NOT NULL",
                &[],
            )
            .await?;

        for row in &rows {
            let datname: &str = row.get(0);
            let blks_hit: i64 = row.get(1);
            let blks_read: i64 = row.get(2);
            let xact_commit: i64 = row.get(3);
            let xact_rollback: i64 = row.get(4);
            let deadlocks: i64 = row.get(5);
            let temp_files: i64 = row.get(6);
            let temp_bytes: i64 = row.get(7);
            let tup_inserted: i64 = row.get(8);
            let tup_updated: i64 = row.get(9);
            let tup_deleted: i64 = row.get(10);
            let tup_returned: i64 = row.get(11);
            let tup_fetched: i64 = row.get(12);

            let mut db_labels = HashMap::new();
            db_labels.insert("datname".into(), datname.to_owned());

            let total_blks = blks_hit + blks_read;
            let cache_hit_ratio = if total_blks > 0 {
                blks_hit as f64 / total_blks as f64
            } else {
                1.0 // No blocks read yet — treat as 100 % cache hit.
            };

            points.push(make_point(
                "pg.cache_hit_ratio",
                cache_hit_ratio,
                MetricKind::Gauge,
                db_labels.clone(),
            ));

            // Counters: callers receive raw cumulative values; the scraper
            // computes deltas before writing to the store.
            points.push(make_point(
                "pg.commits_total",
                xact_commit as f64,
                MetricKind::Counter,
                db_labels.clone(),
            ));
            points.push(make_point(
                "pg.rollbacks_total",
                xact_rollback as f64,
                MetricKind::Counter,
                db_labels.clone(),
            ));
            points.push(make_point(
                "pg.deadlocks_total",
                deadlocks as f64,
                MetricKind::Counter,
                db_labels.clone(),
            ));

            // Temp file spill: non-zero delta per interval means queries are
            // overflowing work_mem and hitting disk.
            points.push(make_point(
                "pg.temp_files_total",
                temp_files as f64,
                MetricKind::Counter,
                db_labels.clone(),
            ));
            points.push(make_point(
                "pg.temp_bytes_total",
                temp_bytes as f64,
                MetricKind::Counter,
                db_labels.clone(),
            ));

            // Tuple DML rates — useful for workload characterisation and
            // predicting autovacuum pressure.
            points.push(make_point(
                "pg.tuples_inserted_total",
                tup_inserted as f64,
                MetricKind::Counter,
                db_labels.clone(),
            ));
            points.push(make_point(
                "pg.tuples_updated_total",
                tup_updated as f64,
                MetricKind::Counter,
                db_labels.clone(),
            ));
            points.push(make_point(
                "pg.tuples_deleted_total",
                tup_deleted as f64,
                MetricKind::Counter,
                db_labels.clone(),
            ));

            // Fetch ratio: tup_fetched (index-scan rows) / tup_returned (all
            // rows scanned).  Low ratio → heavy sequential scan load.
            let fetch_ratio = if tup_returned > 0 {
                tup_fetched as f64 / tup_returned as f64
            } else {
                1.0
            };
            points.push(make_point(
                "pg.tuple_fetch_ratio",
                fetch_ratio,
                MetricKind::Gauge,
                db_labels,
            ));
        }
    }

    // -------------------------------------------------------------------------
    // 3. pg_stat_replication — per-replica write and replay lag (seconds)
    // -------------------------------------------------------------------------
    {
        let rows = client
            .query(
                "SELECT \
                    client_addr::text AS replica_addr, \
                    EXTRACT(EPOCH FROM write_lag) AS write_lag_secs, \
                    EXTRACT(EPOCH FROM replay_lag) AS replay_lag_secs \
                 FROM pg_stat_replication",
                &[],
            )
            .await?;

        for row in &rows {
            // `client_addr` may be NULL for unix-socket standbys.
            let replica_addr: Option<&str> = row.get(0);
            let write_lag: Option<f64> = row.get(1);
            let replay_lag: Option<f64> = row.get(2);

            let label_value = replica_addr.unwrap_or("unknown").to_owned();
            let mut replica_labels = HashMap::new();
            replica_labels.insert("replica_addr".into(), label_value);

            if let Some(wl) = write_lag {
                points.push(make_point(
                    "pg.replication_write_lag_seconds",
                    wl,
                    MetricKind::Gauge,
                    replica_labels.clone(),
                ));
            }
            if let Some(rl) = replay_lag {
                points.push(make_point(
                    "pg.replication_replay_lag_seconds",
                    rl,
                    MetricKind::Gauge,
                    replica_labels,
                ));
            }
        }
    }

    // -------------------------------------------------------------------------
    // 4. Checkpoint counts (Counter)
    // PG17+ moved checkpoint stats to pg_stat_checkpointer; fall back to
    // pg_stat_bgwriter on older versions.
    // -------------------------------------------------------------------------
    {
        let rows = client
            .query(
                "SELECT num_timed, num_requested \
                 FROM pg_stat_checkpointer \
                 UNION ALL \
                 SELECT checkpoints_timed, checkpoints_req \
                 FROM pg_stat_bgwriter \
                 WHERE NOT EXISTS (SELECT 1 FROM pg_stat_checkpointer) \
                 LIMIT 1",
                &[],
            )
            .await
            // If both views fail (very old PG), skip gracefully rather than
            // aborting the entire collection cycle.
            .unwrap_or_default();

        if let Some(row) = rows.first() {
            let timed: i64 = row.get(0);
            let requested: i64 = row.get(1);

            // Expose both counters individually so dashboards can split
            // scheduled vs. pressure-driven checkpoints.
            points.push(make_point(
                "pg.checkpoints_timed_total",
                timed as f64,
                MetricKind::Counter,
                HashMap::new(),
            ));
            points.push(make_point(
                "pg.checkpoints_req_total",
                requested as f64,
                MetricKind::Counter,
                HashMap::new(),
            ));

            // Combined "checkpoint rate" counter = timed + requested.
            points.push(make_point(
                "pg.checkpoint_rate",
                (timed + requested) as f64,
                MetricKind::Counter,
                HashMap::new(),
            ));
        }
    }

    // -------------------------------------------------------------------------
    // 5. pg_stat_wal — WAL throughput metrics (PG 15+; gracefully skipped on
    //    older versions where the view does not exist)
    // -------------------------------------------------------------------------
    {
        let wal_rows = client
            .query(
                "SELECT wal_bytes::bigint, wal_records::bigint, \
                        wal_fpi::bigint, wal_buffers_full::bigint \
                 FROM pg_stat_wal",
                &[],
            )
            .await;

        match wal_rows {
            Ok(rows) => {
                if let Some(row) = rows.first() {
                    let wal_bytes: i64 = row.get(0);
                    let wal_records: i64 = row.get(1);
                    let wal_fpi: i64 = row.get(2);
                    let wal_buffers_full: i64 = row.get(3);

                    points.push(make_point(
                        "pg.wal_bytes_total",
                        wal_bytes as f64,
                        MetricKind::Counter,
                        HashMap::new(),
                    ));
                    points.push(make_point(
                        "pg.wal_records_total",
                        wal_records as f64,
                        MetricKind::Counter,
                        HashMap::new(),
                    ));
                    // Full-page images (FPI): expensive WAL records written
                    // after each checkpoint; high ratio → checkpoint too frequent.
                    points.push(make_point(
                        "pg.wal_fpi_total",
                        wal_fpi as f64,
                        MetricKind::Counter,
                        HashMap::new(),
                    ));
                    // Buffer full events: wal_buffers undersized when non-zero.
                    points.push(make_point(
                        "pg.wal_buffers_full_total",
                        wal_buffers_full as f64,
                        MetricKind::Counter,
                        HashMap::new(),
                    ));
                }
            }
            Err(_) => {
                // pg_stat_wal is only available on PG 15+; skip silently on
                // older versions rather than aborting the whole collection.
                debug!(
                    source_id,
                    engine = "postgres",
                    "pg_stat_wal not available (requires PG 15+); skipping WAL metrics"
                );
            }
        }
    }

    // -------------------------------------------------------------------------
    // 6. pg_stat_user_tables aggregate — live/dead tuple totals across all
    //    user tables.  A rising dead-tuple ratio indicates autovacuum lag.
    // -------------------------------------------------------------------------
    {
        let rows = client
            .query(
                "SELECT \
                    COALESCE(SUM(n_live_tup), 0)::bigint AS total_live, \
                    COALESCE(SUM(n_dead_tup), 0)::bigint AS total_dead, \
                    COALESCE(SUM(seq_scan), 0)::bigint AS total_seq_scans, \
                    COALESCE(SUM(idx_scan), 0)::bigint AS total_idx_scans \
                 FROM pg_stat_user_tables",
                &[],
            )
            .await?;

        if let Some(row) = rows.first() {
            let live: i64 = row.get(0);
            let dead: i64 = row.get(1);
            let seq_scans: i64 = row.get(2);
            let idx_scans: i64 = row.get(3);

            points.push(make_point(
                "pg.tuples_live",
                live as f64,
                MetricKind::Gauge,
                HashMap::new(),
            ));
            points.push(make_point(
                "pg.tuples_dead",
                dead as f64,
                MetricKind::Gauge,
                HashMap::new(),
            ));

            // Dead-tuple ratio: sustained values >0.1 suggest autovacuum is
            // falling behind writes and VACUUM should be tuned.
            let dead_ratio = if live + dead > 0 {
                dead as f64 / (live + dead) as f64
            } else {
                0.0
            };
            points.push(make_point(
                "pg.dead_tuple_ratio",
                dead_ratio,
                MetricKind::Gauge,
                HashMap::new(),
            ));

            // Sequential vs index scan balance across all tables.
            points.push(make_point(
                "pg.seq_scans_total",
                seq_scans as f64,
                MetricKind::Counter,
                HashMap::new(),
            ));
            points.push(make_point(
                "pg.idx_scans_total",
                idx_scans as f64,
                MetricKind::Counter,
                HashMap::new(),
            ));
        }
    }

    // -------------------------------------------------------------------------
    // 7. pg_locks — count of lock requests currently waiting (not granted).
    //    A non-zero value means at least one backend is blocked on another.
    // -------------------------------------------------------------------------
    {
        let rows = client
            .query(
                "SELECT count(*)::bigint \
                 FROM pg_locks \
                 WHERE NOT granted",
                &[],
            )
            .await?;

        if let Some(row) = rows.first() {
            let waiting: i64 = row.get(0);
            points.push(make_point(
                "pg.locks_waiting",
                waiting as f64,
                MetricKind::Gauge,
                HashMap::new(),
            ));
        }
    }

    // -------------------------------------------------------------------------
    // 8. Database size — bytes consumed by each database on disk.
    //    Emitted once per database with a `datname` label, same as section 2.
    //    Useful for capacity planning and detecting unexpected growth spikes.
    // -------------------------------------------------------------------------
    {
        let rows = client
            .query(
                "SELECT datname, pg_database_size(datname)::bigint \
                 FROM pg_database \
                 WHERE datname NOT IN ('template0', 'template1') \
                   AND datname IS NOT NULL",
                &[],
            )
            .await?;

        for row in &rows {
            let datname: &str = row.get(0);
            let size_bytes: i64 = row.get(1);
            let mut db_labels = HashMap::new();
            db_labels.insert("datname".into(), datname.to_owned());
            points.push(make_point(
                "pg.database_size_bytes",
                size_bytes as f64,
                MetricKind::Gauge,
                db_labels,
            ));
        }
    }

    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SourceKind;
    use std::time::Duration;

    #[test]
    fn postgres_collector_engine_name() {
        let col = PostgresCollector::new();
        assert_eq!(col.engine(), "postgres");
    }

    #[tokio::test]
    async fn postgres_collector_returns_empty_on_bad_connection() {
        let col = PostgresCollector::new();
        let config = CollectorConfig {
            source_id: 1,
            source_kind: SourceKind::Database,
            // Use a port that should be unreachable on any CI box.
            connection_string:
                "host=127.0.0.1 port=19999 user=nobody dbname=nobody connect_timeout=1".to_owned(),
            environment: Some("test".to_owned()),
            node_id: None,
            timeout: Duration::from_secs(3),
        };

        let result = col.collect(&config).await;
        // Must not propagate the error — returns empty vec.
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn postgres_collector_returns_empty_on_timeout() {
        // Point at a host that black-holes TCP so the connection hangs.
        // 192.0.2.1 is documentation range (RFC 5737) — guaranteed unreachable.
        let col = PostgresCollector::new();
        let config = CollectorConfig {
            source_id: 2,
            source_kind: SourceKind::Database,
            connection_string: "host=192.0.2.1 port=5432 user=nobody dbname=nobody".to_owned(),
            environment: None,
            node_id: None,
            timeout: Duration::from_millis(100),
        };

        let result = col.collect(&config).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
