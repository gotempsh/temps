// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! MariaDB metric collector.
//!
//! Opens one short-lived `MySqlConnection` per scrape (no pool, no exporter
//! sidecar) and reads everything over plain SQL:
//!
//! 1. `SHOW GLOBAL STATUS` — connections and threads, statement and row
//!    throughput, commits and rollbacks, locks and deadlocks, temporary tables
//!    and full scans, network traffic, the InnoDB buffer pool, redo log and
//!    data I/O, table and file handles, uptime. Only the variables this
//!    collector uses are requested.
//! 2. `SHOW GLOBAL VARIABLES` — the configured limits those values are
//!    measured against (`max_connections`, buffer pool size, table cache, open
//!    files limit, redo log size).
//! 3. `information_schema.SCHEMATA` / `TABLES` — size, table count and
//!    estimated row count per schema. Labels are per schema, never per table,
//!    and capped at [`MAX_SCHEMA_SERIES`] so a server with many schemas cannot
//!    blow up label cardinality.
//! 4. `information_schema.PROCESSLIST` / `INNODB_TRX` — active and idle
//!    connections, long-running queries and open transactions. Read only with
//!    the `PROCESS` privilege: without it the server lists the user's own
//!    threads, which would silently under-report.
//! 5. `SHOW ALL REPLICAS STATUS` (falling back through the older and MySQL
//!    spellings, see [`REPLICA_STATUS_QUERIES`]) — replication lag and thread
//!    state, only when the server is a replica. A server replicating from
//!    several primaries reports its worst connection.
//!
//! The global status read is the only mandatory step. Every other step is
//! skipped on its own when it fails — typically a missing privilege, e.g. the
//! application user may not run `SHOW REPLICA STATUS` — so one denied query
//! never empties the whole batch.
//!
//! Limitation: the processlist and transaction metrics are gated on the
//! `PROCESS` privilege read from `information_schema.USER_PRIVILEGES`, which
//! lists privileges granted to the user directly. A `PROCESS` grant obtained
//! only through a role is not seen, so those metrics are omitted rather than
//! reported partially.
//!
//! Connection and query failures are logged and produce an empty batch, so a
//! slow or unreachable server never blocks the scraper loop.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlRow};
use sqlx::{ColumnIndex, ConnectOptions, Connection, Executor, Row};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::{debug, warn};

use super::{Collector, CollectorConfig};
use crate::error::MetricsError;
use crate::store::{MetricKind, MetricPoint};

const ENGINE: &str = "mariadb";

/// Upper bound on schemas with their own labelled series. The largest schemas
/// are kept; the unlabelled instance totals still cover every schema.
const MAX_SCHEMA_SERIES: usize = 100;

/// Cumulative `SHOW GLOBAL STATUS` counters → metric name. The scraper turns
/// them into per-interval deltas.
///
/// `Questions` counts statements sent by clients, which includes the handful
/// this collector issues per scrape — a constant offset of a few per interval.
///
/// Row operations come from the storage-engine-agnostic `Handler_*` counters:
/// current MariaDB releases no longer have MySQL's `Innodb_rows_*` status
/// variables, while `Handler_*` exist on every MariaDB and MySQL version.
/// Internal temporary tables are counted separately (`Handler_tmp_*`), so these
/// reflect rows of real tables. `Handler_commit`/`Handler_rollback` likewise
/// count transaction ends across storage engines.
const STATUS_COUNTERS: &[(&str, &str)] = &[
    // Statements
    ("questions", "mariadb.queries_total"),
    ("slow_queries", "mariadb.slow_queries_total"),
    ("com_select", "mariadb.statements_select_total"),
    ("com_insert", "mariadb.statements_insert_total"),
    ("com_update", "mariadb.statements_update_total"),
    ("com_delete", "mariadb.statements_delete_total"),
    (
        "max_statement_time_exceeded",
        "mariadb.statements_timed_out_total",
    ),
    // Transactions
    ("handler_commit", "mariadb.commits_total"),
    ("handler_rollback", "mariadb.rollbacks_total"),
    // Connections
    ("connections", "mariadb.connections_created_total"),
    ("threads_created", "mariadb.threads_created_total"),
    ("aborted_connects", "mariadb.aborted_connects_total"),
    ("aborted_clients", "mariadb.aborted_clients_total"),
    ("access_denied_errors", "mariadb.access_denied_errors_total"),
    (
        "connection_errors_max_connections",
        "mariadb.connections_refused_total",
    ),
    // Rows (reads are summed from HANDLER_READS)
    ("handler_write", "mariadb.rows_inserted_total"),
    ("handler_update", "mariadb.rows_updated_total"),
    ("handler_delete", "mariadb.rows_deleted_total"),
    // Locks
    ("innodb_deadlocks", "mariadb.deadlocks_total"),
    ("innodb_row_lock_waits", "mariadb.row_lock_waits_total"),
    ("innodb_row_lock_time", "mariadb.row_lock_wait_ms_total"),
    ("table_locks_waited", "mariadb.table_locks_waited_total"),
    // Query plans
    ("created_tmp_tables", "mariadb.tmp_tables_total"),
    ("created_tmp_disk_tables", "mariadb.tmp_disk_tables_total"),
    ("select_scan", "mariadb.full_scans_total"),
    ("select_full_join", "mariadb.full_joins_total"),
    ("sort_merge_passes", "mariadb.sort_merge_passes_total"),
    ("opened_tables", "mariadb.tables_opened_total"),
    // I/O
    ("bytes_received", "mariadb.network_received_bytes_total"),
    ("bytes_sent", "mariadb.network_sent_bytes_total"),
    ("innodb_data_read", "mariadb.innodb_data_read_bytes_total"),
    (
        "innodb_data_written",
        "mariadb.innodb_data_written_bytes_total",
    ),
    (
        "innodb_os_log_written",
        "mariadb.redo_log_written_bytes_total",
    ),
    ("innodb_log_waits", "mariadb.redo_log_waits_total"),
    (
        "innodb_buffer_pool_wait_free",
        "mariadb.buffer_pool_wait_free_total",
    ),
    ("binlog_bytes_written", "mariadb.binlog_written_bytes_total"),
];

/// Row read requests by access path, summed into `mariadb.rows_read_total`.
const HANDLER_READS: &[&str] = &[
    "handler_read_first",
    "handler_read_key",
    "handler_read_last",
    "handler_read_next",
    "handler_read_prev",
    "handler_read_rnd",
    "handler_read_rnd_next",
];

/// Point-in-time `SHOW GLOBAL STATUS` values → metric name.
///
/// None of these may end in `_total`/`_count`: the query layer charts such
/// names as monotonic counters, which would flatten a steady gauge to 0.
const STATUS_GAUGES: &[(&str, &str)] = &[
    ("threads_connected", "mariadb.connections"),
    ("threads_running", "mariadb.threads_running"),
    ("threads_cached", "mariadb.threads_cached"),
    ("max_used_connections", "mariadb.max_used_connections"),
    ("uptime", "mariadb.uptime_seconds"),
    (
        "innodb_row_lock_current_waits",
        "mariadb.row_lock_waits_current",
    ),
    (
        "innodb_buffer_pool_bytes_data",
        "mariadb.buffer_pool_data_bytes",
    ),
    (
        "innodb_buffer_pool_bytes_dirty",
        "mariadb.buffer_pool_dirty_bytes",
    ),
    ("innodb_checkpoint_age", "mariadb.checkpoint_age_bytes"),
    ("innodb_history_list_length", "mariadb.history_list_length"),
    ("memory_used", "mariadb.memory_used_bytes"),
    ("open_tables", "mariadb.open_tables"),
    ("open_files", "mariadb.open_files"),
];

/// Status variables read only to derive buffer pool ratios.
const BUFFER_POOL_READ_REQUESTS: &str = "innodb_buffer_pool_read_requests";
const BUFFER_POOL_READS: &str = "innodb_buffer_pool_reads";
const BUFFER_POOL_PAGES_TOTAL: &str = "innodb_buffer_pool_pages_total";
const BUFFER_POOL_PAGES_FREE: &str = "innodb_buffer_pool_pages_free";
const BUFFER_POOL_PAGES_DIRTY: &str = "innodb_buffer_pool_pages_dirty";
const DERIVED_ONLY_STATUS: &[&str] = &[
    BUFFER_POOL_READ_REQUESTS,
    BUFFER_POOL_READS,
    BUFFER_POOL_PAGES_TOTAL,
    BUFFER_POOL_PAGES_FREE,
    BUFFER_POOL_PAGES_DIRTY,
];

/// `SHOW GLOBAL VARIABLES` limits → metric name.
const VARIABLE_GAUGES: &[(&str, &str)] = &[
    ("max_connections", "mariadb.max_connections"),
    ("innodb_buffer_pool_size", "mariadb.buffer_pool_size_bytes"),
    ("table_open_cache", "mariadb.table_open_cache"),
    ("open_files_limit", "mariadb.open_files_limit"),
    ("innodb_log_file_size", "mariadb.redo_log_size_bytes"),
];

/// `(status gauge, variable limiting it, ratio metric)`: how close a resource
/// is to its configured limit. Withheld when the limit is 0 or unknown.
const USAGE_RATIOS: &[(&str, &str, &str)] = &[
    (
        "threads_connected",
        "max_connections",
        "mariadb.connections_used_ratio",
    ),
    (
        "open_tables",
        "table_open_cache",
        "mariadb.table_open_cache_usage_ratio",
    ),
    (
        "open_files",
        "open_files_limit",
        "mariadb.open_files_usage_ratio",
    ),
    // InnoDB starts flushing aggressively — stalling writes — as the
    // checkpoint age approaches the redo log capacity.
    (
        "innodb_checkpoint_age",
        "innodb_log_file_size",
        "mariadb.checkpoint_age_ratio",
    ),
];

/// Size, tables and estimated rows per schema that has at least one base
/// table. Views have no storage and are excluded.
///
/// Deliberately not a `SCHEMATA LEFT JOIN TABLES`: that join opens every table
/// of every schema once per schema and made each scrape create ~120 temporary
/// tables (20 on disk) and ~75 commits, which the server's own
/// `Created_tmp_*` and `Handler_*` counters then reported as workload. Two
/// plain reads cost a handful, whatever the schema count.
///
/// `DATA_FREE` is summed per table. InnoDB tables in the shared system
/// tablespace report that tablespace's whole free space, so it can be counted
/// once per such table; file-per-table tablespaces (the default) report their
/// own. Treat it as an upper bound on reclaimable space, not an exact figure.
const SCHEMA_STATS_QUERY: &str = "SELECT TABLE_SCHEMA, \
        CAST(COALESCE(SUM(DATA_LENGTH), 0) AS UNSIGNED), \
        CAST(COALESCE(SUM(INDEX_LENGTH), 0) AS UNSIGNED), \
        CAST(COALESCE(SUM(DATA_FREE), 0) AS UNSIGNED), \
        COUNT(*), \
        CAST(COALESCE(SUM(TABLE_ROWS), 0) AS UNSIGNED) \
     FROM information_schema.TABLES \
     WHERE TABLE_TYPE = 'BASE TABLE' \
       AND TABLE_SCHEMA NOT IN ('information_schema', 'performance_schema', 'mysql', 'sys') \
     GROUP BY TABLE_SCHEMA";

/// Every user schema, so one without tables (a new application database) is
/// still reported, with zeros, rather than missing.
const SCHEMA_NAMES_QUERY: &str = "SELECT SCHEMA_NAME \
     FROM information_schema.SCHEMATA \
     WHERE SCHEMA_NAME NOT IN ('information_schema', 'performance_schema', 'mysql', 'sys')";

/// Whether the connected account holds `PROCESS` directly. `CURRENT_USER()`
/// is `user@host`; `GRANTEE` is `'user'@'host'`.
const PROCESS_PRIVILEGE_QUERY: &str = "SELECT COUNT(*) \
     FROM information_schema.USER_PRIVILEGES \
     WHERE PRIVILEGE_TYPE = 'PROCESS' \
       AND GRANTEE = CONCAT('''', SUBSTRING_INDEX(CURRENT_USER(), '@', 1), '''@''', \
                            SUBSTRING_INDEX(CURRENT_USER(), '@', -1), '''')";

/// Client threads by state, excluding this collector's own connection. A
/// query is long-running after 30 s, the same threshold as
/// `pg.queries_long_running`.
const PROCESSLIST_QUERY: &str = "SELECT \
        CAST(COALESCE(SUM(COMMAND = 'Query'), 0) AS UNSIGNED), \
        CAST(COALESCE(SUM(COMMAND = 'Sleep'), 0) AS UNSIGNED), \
        CAST(COALESCE(SUM(COMMAND = 'Query' AND TIME >= 30), 0) AS UNSIGNED), \
        CAST(COALESCE(MAX(IF(COMMAND = 'Query', TIME, NULL)), 0) AS UNSIGNED) \
     FROM information_schema.PROCESSLIST \
     WHERE ID <> CONNECTION_ID()";

/// Open InnoDB transactions and the age of the oldest one. A transaction left
/// open blocks purge, which `mariadb.history_list_length` then shows growing.
const TRANSACTIONS_QUERY: &str = "SELECT COUNT(*), \
        CAST(COALESCE(MAX(TIMESTAMPDIFF(SECOND, trx_started, NOW())), 0) AS UNSIGNED) \
     FROM information_schema.INNODB_TRX";

/// MariaDB metric collector.
///
/// Stateless apart from the optional fallback credentials: a fresh connection
/// is opened on every [`Collector::collect`] call and closed before it
/// returns.
pub struct MariaDbCollector {
    /// Tried when [`CollectorConfig::connection_string`] cannot log in. The
    /// scraper connects as root first, which sees every schema, every client
    /// thread and the replication status, and falls back to the application
    /// user, which always works but sees only its own database. An imported
    /// server whose root account only accepts local logins needs the fallback.
    fallback_connection_string: Option<String>,
}

impl MariaDbCollector {
    pub fn new() -> Self {
        Self {
            fallback_connection_string: None,
        }
    }

    /// Credentials to try when the primary connection string is rejected.
    pub fn with_fallback_connection_string(mut self, connection_string: impl Into<String>) -> Self {
        self.fallback_connection_string = Some(connection_string.into());
        self
    }

    /// Connect with the primary credentials, then the fallback ones. Logs and
    /// returns `None` when neither works.
    async fn connect(&self, config: &CollectorConfig) -> Option<MySqlConnection> {
        let source_id = config.source_id;
        let primary_error = match open_connection(&config.connection_string).await {
            Ok(conn) => return Some(conn),
            Err(e) => describe_error(&e),
        };

        let Some(fallback) = self
            .fallback_connection_string
            .as_deref()
            .filter(|f| *f != config.connection_string)
        else {
            // SECURITY: never log the connection string — it carries the
            // password. `describe_error` only reports server-sent messages and
            // error kinds.
            warn!(
                source_id,
                engine = ENGINE,
                error = %primary_error,
                "mariadb connection failed; skipping scrape"
            );
            return None;
        };

        debug!(
            source_id,
            engine = ENGINE,
            error = %primary_error,
            "mariadb connection with administrative credentials failed; \
             retrying with the application user"
        );
        match open_connection(fallback).await {
            Ok(conn) => Some(conn),
            Err(e) => {
                warn!(
                    source_id,
                    engine = ENGINE,
                    admin_error = %primary_error,
                    app_user_error = %describe_error(&e),
                    "mariadb connection failed with both the administrative and the \
                     application credentials; skipping scrape"
                );
                None
            }
        }
    }
}

impl Default for MariaDbCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for MariaDbCollector {
    fn engine(&self) -> &'static str {
        ENGINE
    }

    async fn collect(&self, config: &CollectorConfig) -> Result<Vec<MetricPoint>, MetricsError> {
        let source_id = config.source_id;
        debug!(
            source_id,
            engine = ENGINE,
            "starting mariadb metric collection"
        );

        let outcome = tokio::time::timeout(config.timeout, async {
            let mut conn = self.connect(config).await?;
            let snapshot = read_snapshot(&mut conn, source_id).await;
            // Best effort: a failed COM_QUIT only means the server notices the
            // closed socket a moment later.
            let _ = conn.close().await;
            snapshot
        })
        .await;

        match outcome {
            Err(_elapsed) => {
                warn!(
                    source_id,
                    engine = ENGINE,
                    timeout_secs = config.timeout.as_secs(),
                    "mariadb metric collection timed out; returning empty batch"
                );
                Ok(vec![])
            }
            // Already logged by `connect` / `read_snapshot`.
            Ok(None) => Ok(vec![]),
            Ok(Some(snapshot)) => {
                let points = build_points(&snapshot, config);
                debug!(
                    source_id,
                    engine = ENGINE,
                    point_count = points.len(),
                    "mariadb metric collection complete"
                );
                Ok(points)
            }
        }
    }
}

async fn open_connection(connection_string: &str) -> Result<MySqlConnection, sqlx::Error> {
    MySqlConnectOptions::from_str(connection_string)?
        .connect()
        .await
}

/// A loggable description of a connection or query error that cannot leak
/// credentials. Server errors (e.g. `1045 (28000): Access denied for user
/// 'root'@'10.0.0.5'`) are reported verbatim — the server never echoes the
/// password. Client-side errors are reduced to their kind, since some of them
/// render the connection options.
fn describe_error(error: &sqlx::Error) -> String {
    match error {
        sqlx::Error::Database(db) => match db.code() {
            Some(code) => format!("{code}: {}", db.message()),
            None => db.message().to_string(),
        },
        sqlx::Error::Io(e) => format!("io error: {e}"),
        sqlx::Error::Tls(_) => "tls error".to_string(),
        sqlx::Error::Protocol(_) => "protocol error".to_string(),
        sqlx::Error::Configuration(_) => "invalid connection options".to_string(),
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => "connection pool error".to_string(),
        _ => "unexpected client error".to_string(),
    }
}

/// Everything one scrape reads from the server, before it becomes points.
#[derive(Debug, Default, Clone)]
pub(crate) struct MariaDbSnapshot {
    /// Numeric `SHOW GLOBAL STATUS` values keyed by lowercase variable name.
    pub status: HashMap<String, f64>,
    /// Numeric `SHOW GLOBAL VARIABLES` values keyed by lowercase name.
    pub variables: HashMap<String, f64>,
    /// Per-schema storage; empty when the schema query failed.
    pub schemas: Vec<SchemaStats>,
    /// Client thread and transaction activity; `None` without the `PROCESS`
    /// privilege.
    pub activity: Option<ActivityStats>,
    /// `None` when this server is not a replica or the status could not be
    /// read.
    pub replica: Option<ReplicaStatus>,
}

/// Storage of one schema, summed over its base tables. Sizes and row counts
/// come from table statistics: exact for sizes, an estimate for InnoDB rows.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct SchemaStats {
    pub name: String,
    pub data_bytes: u64,
    pub index_bytes: u64,
    /// Allocated but unused space, reclaimable with `OPTIMIZE TABLE`.
    pub free_bytes: u64,
    pub tables: u64,
    pub rows_estimated: u64,
}

impl SchemaStats {
    fn size_bytes(&self) -> u64 {
        self.data_bytes.saturating_add(self.index_bytes)
    }
}

/// Reads one per-schema value.
type SchemaValue = fn(&SchemaStats) -> u64;

/// Per-schema gauges: one series per schema (label `schema`) plus an
/// unlabelled instance total over every schema.
const SCHEMA_METRICS: &[(&str, SchemaValue)] = &[
    ("mariadb.database_size_bytes", SchemaStats::size_bytes),
    ("mariadb.data_size_bytes", |s| s.data_bytes),
    ("mariadb.index_size_bytes", |s| s.index_bytes),
    ("mariadb.data_free_bytes", |s| s.free_bytes),
    ("mariadb.tables", |s| s.tables),
    ("mariadb.rows_estimated", |s| s.rows_estimated),
];

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ActivityStats {
    /// Client threads executing a statement.
    pub connections_active: f64,
    /// Client threads connected but idle.
    pub connections_idle: f64,
    /// Statements running for 30 s or more.
    pub queries_long_running: f64,
    /// Age of the longest-running statement; 0 when none runs.
    pub oldest_query_seconds: f64,
    /// `None` when `INNODB_TRX` could not be read.
    pub transactions: Option<TransactionStats>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct TransactionStats {
    pub open: f64,
    /// Age of the oldest open transaction; 0 when none is open.
    pub oldest_seconds: f64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ReplicaStatus {
    /// Seconds behind the primary; `None` while replication is stopped.
    pub lag_seconds: Option<f64>,
    /// The I/O thread is connected to the primary (`Yes`, not `Connecting`).
    pub io_running: bool,
    pub sql_running: bool,
}

/// Run all metric queries. Returns `None` (after logging) only when the
/// mandatory global status read fails.
async fn read_snapshot(conn: &mut MySqlConnection, source_id: i32) -> Option<MariaDbSnapshot> {
    let status_rows = match conn.fetch_all(status_query().as_str()).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(
                source_id,
                engine = ENGINE,
                error = %describe_error(&e),
                "mariadb SHOW GLOBAL STATUS failed; returning empty batch"
            );
            return None;
        }
    };
    let status = parse_name_value_rows(status_rows.iter().filter_map(name_value));

    let variables = match conn.fetch_all(variables_query().as_str()).await {
        Ok(rows) => parse_name_value_rows(rows.iter().filter_map(name_value)),
        Err(e) => {
            warn!(
                source_id,
                engine = ENGINE,
                error = %describe_error(&e),
                "mariadb SHOW GLOBAL VARIABLES failed; skipping configured limits"
            );
            HashMap::new()
        }
    };

    let schemas = read_schemas(conn, source_id).await;

    let activity = read_activity(conn, source_id).await;
    let replica = read_replica_status(conn, source_id).await;

    Some(MariaDbSnapshot {
        status,
        variables,
        schemas,
        activity,
        replica,
    })
}

/// Storage per schema. The size query is the mandatory half; if only the name
/// list fails, schemas without tables are simply not listed.
async fn read_schemas(conn: &mut MySqlConnection, source_id: i32) -> Vec<SchemaStats> {
    let measured: Vec<SchemaStats> = match conn.fetch_all(SCHEMA_STATS_QUERY).await {
        Ok(rows) => rows.iter().filter_map(schema_stats).collect(),
        Err(e) => {
            warn!(
                source_id,
                engine = ENGINE,
                error = %describe_error(&e),
                "mariadb schema statistics query failed; skipping database sizes"
            );
            return Vec::new();
        }
    };
    let names: Vec<String> = match conn.fetch_all(SCHEMA_NAMES_QUERY).await {
        Ok(rows) => rows.iter().filter_map(|row| column_text(row, 0)).collect(),
        Err(e) => {
            debug!(
                source_id,
                engine = ENGINE,
                error = %describe_error(&e),
                "mariadb schema list query failed; schemas without tables are not reported"
            );
            Vec::new()
        }
    };
    with_empty_schemas(measured, names)
}

/// Add a zeroed entry for every name in `names` that `measured` lacks.
pub(crate) fn with_empty_schemas(
    mut measured: Vec<SchemaStats>,
    names: impl IntoIterator<Item = String>,
) -> Vec<SchemaStats> {
    let mut known: std::collections::HashSet<String> =
        measured.iter().map(|schema| schema.name.clone()).collect();
    for name in names {
        if known.insert(name.clone()) {
            measured.push(SchemaStats {
                name,
                ..Default::default()
            });
        }
    }
    measured
}

fn schema_stats(row: &MySqlRow) -> Option<SchemaStats> {
    let count = |index: usize| column_f64(row, index).map_or(0, |v| v.max(0.0) as u64);
    Some(SchemaStats {
        name: column_text(row, 0)?,
        data_bytes: count(1),
        index_bytes: count(2),
        free_bytes: count(3),
        tables: count(4),
        rows_estimated: count(5),
    })
}

/// Thread and transaction activity, when the account may see every thread.
async fn read_activity(conn: &mut MySqlConnection, source_id: i32) -> Option<ActivityStats> {
    let has_process = match conn.fetch_all(PROCESS_PRIVILEGE_QUERY).await {
        Ok(rows) => rows
            .first()
            .and_then(|row| column_f64(row, 0))
            .is_some_and(|n| n > 0.0),
        Err(e) => {
            debug!(
                source_id,
                engine = ENGINE,
                error = %describe_error(&e),
                "mariadb privilege check failed; skipping connection activity"
            );
            return None;
        }
    };
    if !has_process {
        debug!(
            source_id,
            engine = ENGINE,
            "mariadb account lacks the PROCESS privilege; skipping connection activity"
        );
        return None;
    }

    let row = match conn.fetch_all(PROCESSLIST_QUERY).await {
        Ok(rows) => rows.into_iter().next()?,
        Err(e) => {
            debug!(
                source_id,
                engine = ENGINE,
                error = %describe_error(&e),
                "mariadb process list query failed; skipping connection activity"
            );
            return None;
        }
    };
    let value = |index: usize| column_f64(&row, index).unwrap_or(0.0);
    let mut activity = ActivityStats {
        connections_active: value(0),
        connections_idle: value(1),
        queries_long_running: value(2),
        oldest_query_seconds: value(3),
        transactions: None,
    };

    match conn.fetch_all(TRANSACTIONS_QUERY).await {
        Ok(rows) => {
            activity.transactions = rows.first().map(|row| TransactionStats {
                open: column_f64(row, 0).unwrap_or(0.0),
                oldest_seconds: column_f64(row, 1).unwrap_or(0.0),
            });
        }
        Err(e) => debug!(
            source_id,
            engine = ENGINE,
            error = %describe_error(&e),
            "mariadb INNODB_TRX query failed; skipping open transactions"
        ),
    }
    Some(activity)
}

/// Statements that list replication state, most specific first.
///
/// MariaDB can replicate from several primaries at once, one named connection
/// each; plain `SHOW REPLICA STATUS` only reports the default connection, so
/// the `ALL` forms (MariaDB 10.5.1+ / older spelling) come first. MySQL has no
/// `ALL` form and reports its channels in `SHOW REPLICA STATUS` (8.0.22+) or
/// `SHOW SLAVE STATUS` (removed in 8.4). A server that is not a replica
/// returns no row.
const REPLICA_STATUS_QUERIES: [&str; 4] = [
    "SHOW ALL REPLICAS STATUS",
    "SHOW ALL SLAVES STATUS",
    "SHOW REPLICA STATUS",
    "SHOW SLAVE STATUS",
];

async fn read_replica_status(conn: &mut MySqlConnection, source_id: i32) -> Option<ReplicaStatus> {
    let mut last_error = None;
    for statement in REPLICA_STATUS_QUERIES {
        match conn.fetch_all(statement).await {
            Ok(rows) => {
                let sources: Vec<ReplicaStatus> = rows.iter().map(replica_from_row).collect();
                return aggregate_replicas(&sources);
            }
            Err(e) => last_error = Some(e),
        }
    }
    if let Some(e) = last_error {
        // Usually a missing REPLICA MONITOR / REPLICATION CLIENT privilege
        // when connected as the application user.
        debug!(
            source_id,
            engine = ENGINE,
            error = %describe_error(&e),
            "mariadb replication status unavailable; skipping replication metrics"
        );
    }
    None
}

fn replica_from_row(row: &MySqlRow) -> ReplicaStatus {
    // Column names changed from Slave/Master to Replica/Source over time.
    let running = |names: [&str; 2]| {
        names
            .into_iter()
            .find_map(|name| column_text(row, name))
            .is_some_and(|state| state.trim().eq_ignore_ascii_case("yes"))
    };
    ReplicaStatus {
        lag_seconds: column_f64(row, "Seconds_Behind_Master")
            .or_else(|| column_f64(row, "Seconds_Behind_Source")),
        io_running: running(["Slave_IO_Running", "Replica_IO_Running"]),
        sql_running: running(["Slave_SQL_Running", "Replica_SQL_Running"]),
    }
}

/// One status for a server replicating from several primaries: it is only as
/// healthy as its worst connection, so the lag is the largest known lag and a
/// thread counts as running only if it runs on every connection. `None` when
/// the server is not a replica.
fn aggregate_replicas(sources: &[ReplicaStatus]) -> Option<ReplicaStatus> {
    if sources.is_empty() {
        return None;
    }
    Some(ReplicaStatus {
        lag_seconds: sources
            .iter()
            .filter_map(|source| source.lag_seconds)
            .reduce(f64::max),
        io_running: sources.iter().all(|source| source.io_running),
        sql_running: sources.iter().all(|source| source.sql_running),
    })
}

/// `SHOW GLOBAL STATUS` restricted to the variables this collector reads, so
/// the server does not serialise its ~500 status variables every scrape.
fn status_query() -> String {
    let names = STATUS_COUNTERS
        .iter()
        .chain(STATUS_GAUGES)
        .map(|(variable, _)| *variable)
        .chain(HANDLER_READS.iter().copied())
        .chain(DERIVED_ONLY_STATUS.iter().copied());
    format!(
        "SHOW GLOBAL STATUS WHERE Variable_name IN ({})",
        quoted(names)
    )
}

/// `SHOW GLOBAL VARIABLES` restricted to the limits this collector reports.
fn variables_query() -> String {
    let names = VARIABLE_GAUGES.iter().map(|(variable, _)| *variable);
    format!(
        "SHOW GLOBAL VARIABLES WHERE Variable_name IN ({})",
        quoted(names)
    )
}

/// `'a', 'b'` for an `IN` list of the compile-time variable names above.
fn quoted<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `(Variable_name, Value)` of a `SHOW STATUS` / `SHOW VARIABLES` row.
fn name_value(row: &MySqlRow) -> Option<(String, String)> {
    Some((column_text(row, 0)?, column_text(row, 1)?))
}

/// Keep the numeric values of `SHOW STATUS`-style rows, keyed by lowercase
/// name. Non-numeric values (`ON`, `OFF`, version strings) are dropped.
pub(crate) fn parse_name_value_rows(
    rows: impl IntoIterator<Item = (String, String)>,
) -> HashMap<String, f64> {
    rows.into_iter()
        .filter_map(|(name, value)| {
            let number = value.trim().parse::<f64>().ok().filter(|v| v.is_finite())?;
            Some((name.to_ascii_lowercase(), number))
        })
        .collect()
}

/// A column as text. `SHOW` output is usually `VARCHAR`, but some servers
/// report it with a binary collation, which sqlx only decodes as bytes.
fn column_text<I: ColumnIndex<MySqlRow> + Copy>(row: &MySqlRow, index: I) -> Option<String> {
    if let Ok(value) = row.try_get::<Option<String>, _>(index) {
        return value;
    }
    row.try_get::<Option<Vec<u8>>, _>(index)
        .ok()
        .flatten()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// A column as a number, whatever integer, float or text type the server used
/// for it. `None` for SQL `NULL`, a missing column, or a non-numeric value.
fn column_f64<I: ColumnIndex<MySqlRow> + Copy>(row: &MySqlRow, index: I) -> Option<f64> {
    if let Ok(value) = row.try_get::<Option<u64>, _>(index) {
        return value.map(|v| v as f64);
    }
    if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
        return value.map(|v| v as f64);
    }
    if let Ok(value) = row.try_get::<Option<f64>, _>(index) {
        return value;
    }
    column_text(row, index).and_then(|text| text.trim().parse::<f64>().ok())
}

/// Accumulates the points of one scrape with shared timestamp and labels.
struct Points<'a> {
    config: &'a CollectorConfig,
    time: DateTime<Utc>,
    base_labels: HashMap<String, String>,
    points: Vec<MetricPoint>,
}

impl Points<'_> {
    fn push(&mut self, name: &str, value: f64, kind: MetricKind, schema: Option<&str>) {
        let mut labels = self.base_labels.clone();
        if let Some(schema) = schema {
            labels.insert("schema".into(), schema.to_owned());
        }
        self.points.push(MetricPoint {
            time: self.time,
            source_kind: self.config.source_kind.clone(),
            source_id: self.config.source_id,
            name: name.to_owned(),
            value,
            kind,
            engine: Some(ENGINE.into()),
            environment: self.config.environment.clone(),
            node_id: self.config.node_id,
            labels,
        });
    }

    fn gauge(&mut self, name: &str, value: f64) {
        self.push(name, value, MetricKind::Gauge, None);
    }

    fn counter(&mut self, name: &str, value: f64) {
        self.push(name, value, MetricKind::Counter, None);
    }
}

/// Turn a snapshot into metric points. A variable missing from the snapshot
/// skips the metrics that need it; it never fails the batch.
pub(crate) fn build_points(
    snapshot: &MariaDbSnapshot,
    config: &CollectorConfig,
) -> Vec<MetricPoint> {
    let mut base_labels: HashMap<String, String> = HashMap::new();
    base_labels.insert("engine".into(), ENGINE.into());
    if let Some(env) = &config.environment {
        base_labels.insert("environment".into(), env.clone());
    }
    let schema_series = snapshot.schemas.len().min(MAX_SCHEMA_SERIES) + 1;
    let mut out = Points {
        config,
        time: Utc::now(),
        base_labels,
        points: Vec::with_capacity(96 + SCHEMA_METRICS.len() * schema_series),
    };

    let status = |name: &str| snapshot.status.get(name).copied();
    let variable = |name: &str| snapshot.variables.get(name).copied();

    for (name, metric) in STATUS_GAUGES {
        if let Some(value) = status(name) {
            out.gauge(metric, value);
        }
    }
    for (name, metric) in STATUS_COUNTERS {
        if let Some(value) = status(name) {
            out.counter(metric, value);
        }
    }
    let reads: Vec<f64> = HANDLER_READS.iter().filter_map(|v| status(v)).collect();
    if !reads.is_empty() {
        out.counter("mariadb.rows_read_total", reads.iter().sum());
    }

    for (name, metric) in VARIABLE_GAUGES {
        if let Some(value) = variable(name) {
            out.gauge(metric, value);
        }
    }
    for (used, limit, metric) in USAGE_RATIOS {
        if let (Some(used), Some(limit)) = (status(used), variable(limit)) {
            if limit > 0.0 {
                out.gauge(metric, used / limit);
            }
        }
    }

    push_buffer_pool_ratios(&mut out, &status);

    if let Some(activity) = &snapshot.activity {
        out.gauge("mariadb.connections_active", activity.connections_active);
        out.gauge("mariadb.connections_idle", activity.connections_idle);
        out.gauge(
            "mariadb.queries_long_running",
            activity.queries_long_running,
        );
        out.gauge(
            "mariadb.oldest_query_seconds",
            activity.oldest_query_seconds,
        );
        if let Some(transactions) = &activity.transactions {
            out.gauge("mariadb.transactions_open", transactions.open);
            out.gauge(
                "mariadb.oldest_transaction_seconds",
                transactions.oldest_seconds,
            );
        }
    }

    if let Some(replica) = &snapshot.replica {
        if let Some(lag) = replica.lag_seconds {
            out.gauge("mariadb.replication_lag_seconds", lag);
        }
        out.gauge("mariadb.replica_io_running", flag(replica.io_running));
        out.gauge("mariadb.replica_sql_running", flag(replica.sql_running));
    }

    push_schema_points(&mut out, &snapshot.schemas);

    out.points
}

fn flag(value: bool) -> f64 {
    if value {
        1.0
    } else {
        0.0
    }
}

/// Buffer pool hit ratio, fill and dirty share.
fn push_buffer_pool_ratios(out: &mut Points<'_>, status: &impl Fn(&str) -> Option<f64>) {
    // Share of logical buffer pool reads served from memory over the server's
    // lifetime. `Innodb_buffer_pool_reads` counts the requests that had to go
    // to disk.
    //
    // Pages InnoDB loads on its own at startup (dictionary, recovery, buffer
    // pool restore) count as disk reads without a logical request, so a fresh
    // server reports more disk reads than requests — 152 against 75 on a
    // just-started 12.3. The formula would read 0 % there, which looks like a
    // thrashing cache, so the ratio is withheld until requests cover them.
    if let (Some(requests), Some(disk_reads)) =
        (status(BUFFER_POOL_READ_REQUESTS), status(BUFFER_POOL_READS))
    {
        let ratio = if requests == 0.0 && disk_reads == 0.0 {
            Some(1.0) // Nothing read yet — treat as a 100 % hit, like Postgres.
        } else if disk_reads <= requests {
            Some(1.0 - disk_reads / requests)
        } else {
            None
        };
        if let Some(ratio) = ratio {
            out.gauge("mariadb.buffer_pool_hit_ratio", ratio);
        }
        // Lifetime read requests behind the ratio: alert rules on it are only
        // evaluated once there is enough volume to mean something. A gauge
        // (not a `_total` counter) because the evaluator needs the absolute
        // value, like `pg.cache_blocks_accessed`.
        out.gauge("mariadb.buffer_pool_read_requests", requests);
    }

    if let Some(total) = status(BUFFER_POOL_PAGES_TOTAL).filter(|t| *t > 0.0) {
        if let Some(free) = status(BUFFER_POOL_PAGES_FREE) {
            out.gauge(
                "mariadb.buffer_pool_usage_ratio",
                ((total - free) / total).clamp(0.0, 1.0),
            );
        }
        if let Some(dirty) = status(BUFFER_POOL_PAGES_DIRTY) {
            out.gauge(
                "mariadb.buffer_pool_dirty_ratio",
                (dirty / total).clamp(0.0, 1.0),
            );
        }
    }
}

/// One series per schema (largest first, capped) plus an unlabelled instance
/// total over every schema for the stat tiles.
fn push_schema_points(out: &mut Points<'_>, schemas: &[SchemaStats]) {
    if schemas.is_empty() {
        return;
    }
    let mut largest: Vec<&SchemaStats> = schemas.iter().collect();
    largest.sort_by(|a, b| {
        b.size_bytes()
            .cmp(&a.size_bytes())
            .then_with(|| a.name.cmp(&b.name))
    });
    for schema in largest.into_iter().take(MAX_SCHEMA_SERIES) {
        for (metric, value) in SCHEMA_METRICS {
            out.push(
                metric,
                value(schema) as f64,
                MetricKind::Gauge,
                Some(&schema.name),
            );
        }
    }
    for (metric, value) in SCHEMA_METRICS {
        let total = schemas
            .iter()
            .fold(0u64, |acc, schema| acc.saturating_add(value(schema)));
        out.gauge(metric, total as f64);
    }
}

#[cfg(test)]
mod tests {
    fn replica(lag: Option<f64>, io: bool, sql: bool) -> ReplicaStatus {
        ReplicaStatus {
            lag_seconds: lag,
            io_running: io,
            sql_running: sql,
        }
    }

    #[test]
    fn aggregate_replicas_is_none_for_a_server_that_is_not_a_replica() {
        assert_eq!(aggregate_replicas(&[]), None);
    }

    #[test]
    fn aggregate_replicas_passes_a_single_source_through() {
        let source = replica(Some(3.0), true, true);
        assert_eq!(aggregate_replicas(&[source]), Some(source));
    }

    #[test]
    fn aggregate_replicas_reports_the_worst_of_several_sources() {
        let merged = aggregate_replicas(&[
            replica(Some(2.0), true, true),
            replica(Some(40.0), true, false),
            replica(None, false, true),
        ]);
        assert_eq!(merged, Some(replica(Some(40.0), false, false)));
    }

    #[test]
    fn aggregate_replicas_has_no_lag_while_every_source_is_stopped() {
        let merged = aggregate_replicas(&[replica(None, false, false), replica(None, true, false)]);
        assert_eq!(merged, Some(replica(None, false, false)));
    }

    use super::*;
    use crate::store::SourceKind;
    use std::time::Duration;

    fn config() -> CollectorConfig {
        CollectorConfig::new(9, SourceKind::Database, "mysql://unused")
            .with_environment("production")
            .with_node_id(3)
    }

    fn rows(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    fn full_status() -> HashMap<String, f64> {
        parse_name_value_rows(rows(&[
            ("Threads_connected", "12"),
            ("Threads_running", "3"),
            ("Max_used_connections", "40"),
            ("Uptime", "86400"),
            ("Open_tables", "200"),
            ("Open_files", "50"),
            ("Innodb_checkpoint_age", "25165824"),
            ("Questions", "123456"),
            ("Slow_queries", "7"),
            ("Com_select", "9000"),
            ("Handler_commit", "800"),
            ("Handler_rollback", "6"),
            ("Innodb_deadlocks", "1"),
            ("Created_tmp_disk_tables", "11"),
            ("Bytes_sent", "1048576"),
            ("Aborted_connects", "2"),
            ("Aborted_clients", "5"),
            ("Handler_read_key", "600"),
            ("Handler_read_next", "300"),
            ("Handler_read_rnd_next", "100"),
            ("Handler_write", "200"),
            ("Handler_update", "30"),
            ("Handler_delete", "4"),
            ("Innodb_buffer_pool_read_requests", "10000"),
            ("Innodb_buffer_pool_reads", "100"),
            ("Innodb_buffer_pool_pages_total", "8000"),
            ("Innodb_buffer_pool_pages_free", "2000"),
            ("Innodb_buffer_pool_pages_dirty", "400"),
        ]))
    }

    fn full_variables() -> HashMap<String, f64> {
        parse_name_value_rows(rows(&[
            ("max_connections", "150"),
            ("innodb_buffer_pool_size", "134217728"),
            ("table_open_cache", "2000"),
            ("open_files_limit", "1000"),
            ("innodb_log_file_size", "100663296"),
        ]))
    }

    fn point<'a>(points: &'a [MetricPoint], name: &str) -> Option<&'a MetricPoint> {
        points
            .iter()
            .find(|p| p.name == name && !p.labels.contains_key("schema"))
    }

    fn schema(name: &str, data_bytes: u64) -> SchemaStats {
        SchemaStats {
            name: name.to_string(),
            data_bytes,
            ..Default::default()
        }
    }

    #[test]
    fn status_rows_are_keyed_lowercase_and_non_numeric_values_dropped() {
        let parsed = parse_name_value_rows(rows(&[
            ("Threads_connected", "12"),
            ("Uptime", " 3600 "),
            (
                "Innodb_buffer_pool_dump_status",
                "Dumping of buffer pool not started",
            ),
            ("Ssl_cipher", ""),
            ("Rpl_status", "OFF"),
        ]));

        assert_eq!(parsed.get("threads_connected"), Some(&12.0));
        assert_eq!(parsed.get("uptime"), Some(&3600.0));
        assert_eq!(
            parsed.len(),
            2,
            "non-numeric values must be dropped: {parsed:?}"
        );
    }

    #[test]
    fn status_query_requests_every_variable_it_reads() {
        let query = status_query();
        for (variable, _) in STATUS_COUNTERS.iter().chain(STATUS_GAUGES) {
            assert!(
                query.contains(&format!("'{variable}'")),
                "{variable} missing"
            );
        }
        for variable in HANDLER_READS.iter().chain(DERIVED_ONLY_STATUS) {
            assert!(
                query.contains(&format!("'{variable}'")),
                "{variable} missing"
            );
        }
        for (used, _, _) in USAGE_RATIOS {
            assert!(query.contains(&format!("'{used}'")), "{used} missing");
        }
    }

    #[test]
    fn variables_query_requests_every_limit_it_reads() {
        let query = variables_query();
        for (variable, _) in VARIABLE_GAUGES {
            assert!(
                query.contains(&format!("'{variable}'")),
                "{variable} missing"
            );
        }
        for (_, limit, _) in USAGE_RATIOS {
            assert!(query.contains(&format!("'{limit}'")), "{limit} missing");
        }
    }

    #[test]
    fn every_metric_name_is_unique() {
        let mut names: Vec<&str> = STATUS_COUNTERS
            .iter()
            .chain(STATUS_GAUGES)
            .chain(VARIABLE_GAUGES)
            .map(|(_, metric)| *metric)
            .chain(USAGE_RATIOS.iter().map(|(_, _, metric)| *metric))
            .chain(SCHEMA_METRICS.iter().map(|(metric, _)| *metric))
            .collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "a metric name is mapped twice");
    }

    #[test]
    fn builds_gauges_counters_and_derived_ratios() {
        let snapshot = MariaDbSnapshot {
            status: full_status(),
            variables: full_variables(),
            ..Default::default()
        };

        let points = build_points(&snapshot, &config());

        let value = |name: &str| point(&points, name).map(|p| p.value);
        let kind = |name: &str| point(&points, name).map(|p| p.kind.clone());

        for (name, expected) in [
            ("mariadb.connections", 12.0),
            ("mariadb.max_connections", 150.0),
            ("mariadb.connections_used_ratio", 12.0 / 150.0),
            ("mariadb.threads_running", 3.0),
            ("mariadb.uptime_seconds", 86400.0),
            ("mariadb.buffer_pool_hit_ratio", 0.99),
            ("mariadb.buffer_pool_read_requests", 10000.0),
            ("mariadb.buffer_pool_size_bytes", 134217728.0),
            ("mariadb.buffer_pool_usage_ratio", 0.75),
            ("mariadb.buffer_pool_dirty_ratio", 0.05),
            ("mariadb.table_open_cache_usage_ratio", 0.1),
            ("mariadb.open_files_usage_ratio", 0.05),
            ("mariadb.checkpoint_age_ratio", 0.25),
        ] {
            assert_eq!(value(name), Some(expected), "{name}");
            assert_eq!(kind(name), Some(MetricKind::Gauge), "{name}");
        }

        for (name, expected) in [
            ("mariadb.queries_total", 123456.0),
            ("mariadb.slow_queries_total", 7.0),
            ("mariadb.statements_select_total", 9000.0),
            ("mariadb.commits_total", 800.0),
            ("mariadb.rollbacks_total", 6.0),
            ("mariadb.deadlocks_total", 1.0),
            ("mariadb.tmp_disk_tables_total", 11.0),
            ("mariadb.network_sent_bytes_total", 1048576.0),
            ("mariadb.aborted_connects_total", 2.0),
            ("mariadb.aborted_clients_total", 5.0),
            ("mariadb.rows_read_total", 1000.0),
            ("mariadb.rows_inserted_total", 200.0),
            ("mariadb.rows_updated_total", 30.0),
            ("mariadb.rows_deleted_total", 4.0),
        ] {
            assert_eq!(value(name), Some(expected), "{name}");
            assert_eq!(
                kind(name),
                Some(MetricKind::Counter),
                "{name} is cumulative and must be delta-ed by the scraper"
            );
        }

        for absent in [
            "mariadb.replication_lag_seconds",
            "mariadb.replica_io_running",
            "mariadb.connections_active",
            "mariadb.transactions_open",
        ] {
            assert!(
                point(&points, absent).is_none(),
                "{absent} needs data this snapshot does not have"
            );
        }
        let first = &points[0];
        assert_eq!(first.source_id, 9);
        assert_eq!(first.node_id, Some(3));
        assert_eq!(first.engine.as_deref(), Some("mariadb"));
        assert_eq!(
            first.labels.get("environment").map(String::as_str),
            Some("production")
        );
    }

    #[test]
    fn gauge_names_are_not_treated_as_monotonic_counters() {
        // Every variable the collector reads, so every metric is emitted.
        let status: HashMap<String, f64> = STATUS_COUNTERS
            .iter()
            .chain(STATUS_GAUGES)
            .map(|(variable, _)| *variable)
            .chain(HANDLER_READS.iter().copied())
            .chain(DERIVED_ONLY_STATUS.iter().copied())
            .map(|variable| (variable.to_string(), 1.0))
            .collect();
        let variables: HashMap<String, f64> = VARIABLE_GAUGES
            .iter()
            .map(|(variable, _)| (variable.to_string(), 2.0))
            .collect();
        let snapshot = MariaDbSnapshot {
            status,
            variables,
            schemas: vec![schema("app", 1)],
            activity: Some(ActivityStats {
                transactions: Some(TransactionStats::default()),
                ..Default::default()
            }),
            replica: Some(ReplicaStatus {
                lag_seconds: Some(1.0),
                io_running: true,
                sql_running: true,
            }),
        };
        let points = build_points(&snapshot, &config());
        let expected_metrics = STATUS_COUNTERS.len()
            + STATUS_GAUGES.len()
            + VARIABLE_GAUGES.len()
            + USAGE_RATIOS.len()
            + 2 * SCHEMA_METRICS.len()
            + 1 // rows_read_total
            + 4 // buffer pool hit ratio, read requests, usage, dirty
            + 6 // activity
            + 3; // replica
        assert_eq!(points.len(), expected_metrics);
        for p in points {
            assert_eq!(
                crate::store::is_monotonic_counter(&p.name),
                p.kind == MetricKind::Counter,
                "{} naming must match its kind",
                p.name
            );
        }
    }

    #[test]
    fn missing_variables_skip_only_the_metrics_that_need_them() {
        // A server reporting only a few status variables (no handler or
        // buffer pool disk-read counters) and a failed SHOW GLOBAL VARIABLES.
        let status = parse_name_value_rows(rows(&[
            ("Threads_connected", "4"),
            ("Questions", "10"),
            ("Innodb_buffer_pool_read_requests", "50"),
            ("Innodb_buffer_pool_pages_total", "0"),
            ("Innodb_buffer_pool_pages_free", "0"),
        ]));
        let snapshot = MariaDbSnapshot {
            status,
            ..Default::default()
        };

        let points = build_points(&snapshot, &config());
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();

        assert_eq!(names, vec!["mariadb.connections", "mariadb.queries_total"]);
    }

    #[test]
    fn buffer_pool_hit_ratio_is_full_before_any_read() {
        let status = parse_name_value_rows(rows(&[
            ("Innodb_buffer_pool_read_requests", "0"),
            ("Innodb_buffer_pool_reads", "0"),
        ]));
        let snapshot = MariaDbSnapshot {
            status,
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        assert_eq!(
            point(&points, "mariadb.buffer_pool_hit_ratio").map(|p| p.value),
            Some(1.0)
        );
    }

    /// Regression: a just-started MariaDB 12.3 reported 152 buffer pool disk
    /// reads against 75 read requests (startup page loads), which the plain
    /// formula turned into a 0 % hit ratio.
    #[test]
    fn startup_disk_reads_do_not_report_a_zero_hit_ratio() {
        let status = parse_name_value_rows(rows(&[
            ("Innodb_buffer_pool_read_requests", "75"),
            ("Innodb_buffer_pool_reads", "152"),
        ]));
        let snapshot = MariaDbSnapshot {
            status,
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        assert!(point(&points, "mariadb.buffer_pool_hit_ratio").is_none());
        assert_eq!(
            point(&points, "mariadb.buffer_pool_read_requests").map(|p| p.value),
            Some(75.0),
            "the volume behind the ratio is still reported"
        );
    }

    /// Regression: current MariaDB has no `Innodb_rows_*` status variables, so
    /// row operations were silently never reported. They come from the
    /// `Handler_*` counters instead; reads sum every access path.
    #[test]
    fn row_operations_come_from_handler_counters() {
        let status = parse_name_value_rows(rows(&[
            ("Handler_read_first", "1"),
            ("Handler_read_key", "20"),
            ("Handler_read_rnd_next", "54"),
            ("Handler_write", "2"),
            ("Handler_update", "0"),
            ("Handler_delete", "0"),
        ]));
        let snapshot = MariaDbSnapshot {
            status,
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        let value = |name: &str| point(&points, name).map(|p| p.value);
        assert_eq!(value("mariadb.rows_read_total"), Some(75.0));
        assert_eq!(value("mariadb.rows_inserted_total"), Some(2.0));
        assert_eq!(value("mariadb.rows_updated_total"), Some(0.0));
        assert_eq!(value("mariadb.rows_deleted_total"), Some(0.0));
    }

    #[test]
    fn zero_limits_emit_no_usage_ratio() {
        let snapshot = MariaDbSnapshot {
            status: parse_name_value_rows(rows(&[("Threads_connected", "1"), ("Open_files", "5")])),
            variables: HashMap::from([
                ("max_connections".to_string(), 0.0),
                ("open_files_limit".to_string(), 0.0),
            ]),
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        assert!(point(&points, "mariadb.connections_used_ratio").is_none());
        assert!(point(&points, "mariadb.open_files_usage_ratio").is_none());
    }

    #[test]
    fn connection_activity_is_reported_with_the_process_privilege() {
        let snapshot = MariaDbSnapshot {
            activity: Some(ActivityStats {
                connections_active: 4.0,
                connections_idle: 20.0,
                queries_long_running: 1.0,
                oldest_query_seconds: 95.0,
                transactions: Some(TransactionStats {
                    open: 2.0,
                    oldest_seconds: 700.0,
                }),
            }),
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        let value = |name: &str| point(&points, name).map(|p| p.value);
        assert_eq!(value("mariadb.connections_active"), Some(4.0));
        assert_eq!(value("mariadb.connections_idle"), Some(20.0));
        assert_eq!(value("mariadb.queries_long_running"), Some(1.0));
        assert_eq!(value("mariadb.oldest_query_seconds"), Some(95.0));
        assert_eq!(value("mariadb.transactions_open"), Some(2.0));
        assert_eq!(value("mariadb.oldest_transaction_seconds"), Some(700.0));
    }

    #[test]
    fn replication_lag_and_thread_state_are_reported_for_replicas() {
        let snapshot = MariaDbSnapshot {
            replica: Some(ReplicaStatus {
                lag_seconds: Some(42.0),
                io_running: true,
                sql_running: true,
            }),
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        let value = |name: &str| point(&points, name).map(|p| p.value);
        assert_eq!(value("mariadb.replication_lag_seconds"), Some(42.0));
        assert_eq!(value("mariadb.replica_io_running"), Some(1.0));
        assert_eq!(value("mariadb.replica_sql_running"), Some(1.0));
    }

    /// A stopped SQL thread reports no lag at all (`NULL`); the thread state
    /// is what tells the operator replication is broken.
    #[test]
    fn a_stopped_replica_reports_its_threads_without_lag() {
        let snapshot = MariaDbSnapshot {
            replica: Some(ReplicaStatus {
                lag_seconds: None,
                io_running: true,
                sql_running: false,
            }),
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        let value = |name: &str| point(&points, name).map(|p| p.value);
        assert_eq!(value("mariadb.replication_lag_seconds"), None);
        assert_eq!(value("mariadb.replica_io_running"), Some(1.0));
        assert_eq!(value("mariadb.replica_sql_running"), Some(0.0));
    }

    #[test]
    fn schema_statistics_are_per_schema_plus_instance_totals() {
        let snapshot = MariaDbSnapshot {
            schemas: vec![
                SchemaStats {
                    name: "app".to_string(),
                    data_bytes: 2000,
                    index_bytes: 1000,
                    free_bytes: 500,
                    tables: 3,
                    rows_estimated: 120,
                },
                SchemaStats {
                    name: "analytics".to_string(),
                    data_bytes: 1000,
                    index_bytes: 0,
                    free_bytes: 0,
                    tables: 1,
                    rows_estimated: 30,
                },
                // A new, empty application database still shows up.
                schema("fresh", 0),
            ],
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        let value_of = |name: &str, schema: Option<&str>| {
            points
                .iter()
                .find(|p| p.name == name && p.labels.get("schema").map(String::as_str) == schema)
                .map(|p| p.value)
        };
        assert_eq!(
            value_of("mariadb.database_size_bytes", Some("app")),
            Some(3000.0)
        );
        assert_eq!(
            value_of("mariadb.data_size_bytes", Some("app")),
            Some(2000.0)
        );
        assert_eq!(
            value_of("mariadb.index_size_bytes", Some("app")),
            Some(1000.0)
        );
        assert_eq!(
            value_of("mariadb.data_free_bytes", Some("app")),
            Some(500.0)
        );
        assert_eq!(value_of("mariadb.tables", Some("app")), Some(3.0));
        assert_eq!(value_of("mariadb.rows_estimated", Some("app")), Some(120.0));
        assert_eq!(
            value_of("mariadb.database_size_bytes", Some("analytics")),
            Some(1000.0)
        );
        assert_eq!(value_of("mariadb.tables", Some("fresh")), Some(0.0));

        assert_eq!(value_of("mariadb.database_size_bytes", None), Some(4000.0));
        assert_eq!(value_of("mariadb.tables", None), Some(4.0));
        assert_eq!(value_of("mariadb.rows_estimated", None), Some(150.0));
    }

    #[test]
    fn schemas_without_tables_are_added_with_zeros() {
        let measured = vec![SchemaStats {
            name: "app".to_string(),
            data_bytes: 10,
            tables: 2,
            ..Default::default()
        }];
        let merged = with_empty_schemas(
            measured,
            ["app".to_string(), "fresh".to_string(), "fresh".to_string()],
        );
        let names: Vec<&str> = merged.iter().map(|s| s.name.as_str()).collect();
        // `app` keeps its measured values; neither name is duplicated.
        assert_eq!(names, vec!["app", "fresh"]);
        assert_eq!(merged[0].data_bytes, 10);
        assert_eq!(
            merged[1],
            SchemaStats {
                name: "fresh".to_string(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn schema_series_are_capped_but_the_totals_cover_every_schema() {
        let schemas: Vec<SchemaStats> = (0..(MAX_SCHEMA_SERIES as u64 + 20))
            .map(|i| schema(&format!("tenant_{i:04}"), i + 1))
            .collect();
        let expected_total: u64 = schemas.iter().map(|s| s.data_bytes).sum();
        let snapshot = MariaDbSnapshot {
            schemas,
            ..Default::default()
        };

        let points = build_points(&snapshot, &config());
        let labelled: Vec<&MetricPoint> = points
            .iter()
            .filter(|p| p.name == "mariadb.database_size_bytes" && p.labels.contains_key("schema"))
            .collect();

        assert_eq!(labelled.len(), MAX_SCHEMA_SERIES);
        assert_eq!(
            points
                .iter()
                .filter(|p| p.labels.contains_key("schema"))
                .count(),
            MAX_SCHEMA_SERIES * SCHEMA_METRICS.len()
        );
        assert!(
            labelled.iter().all(|p| p.value > 20.0),
            "the smallest schemas are the ones dropped"
        );
        assert_eq!(
            point(&points, "mariadb.database_size_bytes").map(|p| p.value),
            Some(expected_total as f64)
        );
    }

    #[test]
    fn engine_name() {
        assert_eq!(MariaDbCollector::new().engine(), "mariadb");
    }

    #[tokio::test]
    async fn returns_empty_on_refused_connection() {
        let config = CollectorConfig::new(
            1,
            SourceKind::Database,
            "mysql://nobody:secret@127.0.0.1:19998/",
        )
        .with_timeout(Duration::from_secs(3));
        let result = MariaDbCollector::new()
            .with_fallback_connection_string("mysql://app:secret@127.0.0.1:19998/")
            .collect(&config)
            .await;
        assert!(matches!(result, Ok(points) if points.is_empty()));
    }

    #[tokio::test]
    async fn returns_empty_on_timeout() {
        // 192.0.2.1 is TEST-NET-1 (RFC 5737): connections hang.
        let config =
            CollectorConfig::new(2, SourceKind::Database, "mysql://nobody:x@192.0.2.1:3306/")
                .with_timeout(Duration::from_millis(100));
        let started = std::time::Instant::now();
        let result = MariaDbCollector::new().collect(&config).await;
        assert!(matches!(result, Ok(points) if points.is_empty()));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Server counters a scrape could inflate, keyed by lowercase name.
    async fn status_counters(conn: &mut MySqlConnection) -> HashMap<String, i64> {
        let rows = conn
            .fetch_all(
                "SHOW GLOBAL STATUS WHERE Variable_name IN \
                 ('Created_tmp_tables', 'Created_tmp_disk_tables', 'Handler_commit')",
            )
            .await
            .expect("read status counters");
        rows.iter()
            .filter_map(name_value)
            .filter_map(|(name, value)| Some((name.to_ascii_lowercase(), value.parse().ok()?)))
            .collect()
    }

    /// Scrapes a real `mariadb` container: as root, and as the application
    /// user after a rejected root login (the path an imported server whose
    /// root only accepts local logins takes).
    #[tokio::test]
    async fn scrapes_a_real_mariadb_server() {
        use testcontainers::{
            core::{ContainerPort, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };

        // Reserved URI characters prove the scraper's encoding round-trips.
        const ROOT_PASSWORD: &str = "r00t:p@ss/w#rd?1";

        let container = match GenericImage::new("mariadb", "lts")
            .with_exposed_port(ContainerPort::Tcp(3306))
            .with_wait_for(WaitFor::message_on_stderr("ready for connections"))
            .with_env_var("MARIADB_ROOT_PASSWORD", ROOT_PASSWORD)
            .with_env_var("MARIADB_DATABASE", "appdb")
            .with_env_var("MARIADB_USER", "app")
            .with_env_var("MARIADB_PASSWORD", "app-secret")
            .start()
            .await
        {
            Ok(container) => container,
            Err(e) => {
                println!("Docker not available, skipping: {e}");
                return;
            }
        };
        let (Ok(host), Ok(port)) = (
            container.get_host().await,
            container.get_host_port_ipv4(3306).await,
        ) else {
            println!("Could not resolve the test container's address, skipping");
            return;
        };
        // Encoded exactly as the scraper encodes stored credentials.
        let root_url = format!(
            "mysql://root:{}@{host}:{port}/",
            crate::scraper::urlencoded(ROOT_PASSWORD)
        );
        let app_url = format!("mysql://app:app-secret@{host}:{port}/");

        // The entrypoint runs a temporary, socket-only server first and logs
        // "ready for connections" for it too; retry until TCP accepts.
        let mut setup = None;
        // Each attempt is bounded: a published port that accepts TCP but never
        // sends the server greeting must fail the test, not hang it.
        for _ in 0..60 {
            if let Ok(Ok(conn)) =
                tokio::time::timeout(Duration::from_secs(2), open_connection(&app_url)).await
            {
                setup = Some(conn);
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut setup = setup.expect("test mariadb must accept connections");
        setup
            .execute("CREATE TABLE appdb.items (id INT PRIMARY KEY, body TEXT) ENGINE=InnoDB")
            .await
            .expect("create table");
        setup
            .execute("INSERT INTO appdb.items VALUES (1, 'a'), (2, 'b'), (3, 'c')")
            .await
            .expect("insert rows");
        // Left open with an uncommitted transaction, so root sees an active
        // InnoDB transaction and an idle client thread.
        setup.execute("BEGIN").await.expect("begin");
        setup
            .execute("UPDATE appdb.items SET body = 'z' WHERE id = 1")
            .await
            .expect("update in open transaction");

        let config = CollectorConfig::new(77, SourceKind::Database, root_url.clone())
            .with_timeout(Duration::from_secs(10));
        let mut probe = open_connection(&root_url).await.expect("root probe");
        let before = status_counters(&mut probe).await;
        let points = MariaDbCollector::new()
            .collect(&config)
            .await
            .expect("collect never errors");
        let after = status_counters(&mut probe).await;
        let _ = probe.close().await;
        // The server's own counters are charted as workload, so a scrape must
        // not inflate them: the schema statistics once joined SCHEMATA to
        // TABLES and cost ~120 temporary tables (~20 on disk) and ~75 commits
        // each time.
        for (name, limit) in [
            ("created_tmp_tables", 25),
            ("created_tmp_disk_tables", 5),
            ("handler_commit", 10),
        ] {
            let spent = after[name] - before[name];
            assert!(
                spent <= limit,
                "one scrape cost {spent} {name} (limit {limit})"
            );
        }
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        for expected in [
            "mariadb.connections",
            "mariadb.max_connections",
            "mariadb.connections_used_ratio",
            "mariadb.threads_running",
            "mariadb.uptime_seconds",
            "mariadb.queries_total",
            "mariadb.slow_queries_total",
            "mariadb.statements_select_total",
            "mariadb.commits_total",
            "mariadb.deadlocks_total",
            "mariadb.row_lock_waits_total",
            "mariadb.tmp_disk_tables_total",
            "mariadb.network_sent_bytes_total",
            "mariadb.redo_log_written_bytes_total",
            "mariadb.aborted_connects_total",
            "mariadb.rows_read_total",
            "mariadb.rows_inserted_total",
            "mariadb.buffer_pool_size_bytes",
            "mariadb.buffer_pool_usage_ratio",
            "mariadb.buffer_pool_dirty_ratio",
            "mariadb.history_list_length",
            "mariadb.checkpoint_age_ratio",
            "mariadb.table_open_cache_usage_ratio",
            "mariadb.open_files_usage_ratio",
            "mariadb.database_size_bytes",
            "mariadb.tables",
            "mariadb.connections_active",
            "mariadb.connections_idle",
            "mariadb.transactions_open",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
        let value = |name: &str| point(&points, name).map(|p| p.value);
        assert!(
            value("mariadb.connections").is_some_and(|c| c >= 2.0),
            "the collector's and the setup connection are counted"
        );
        assert!(
            value("mariadb.connections_idle").is_some_and(|c| c >= 1.0),
            "the setup connection idles between statements"
        );
        assert!(
            value("mariadb.transactions_open").is_some_and(|t| t >= 1.0),
            "the setup connection holds an open transaction"
        );
        let appdb = |name: &str| {
            points
                .iter()
                .find(|p| {
                    p.name == name && p.labels.get("schema").map(String::as_str) == Some("appdb")
                })
                .map(|p| p.value)
        };
        assert!(
            appdb("mariadb.database_size_bytes").is_some_and(|s| s > 0.0),
            "appdb holds an InnoDB table"
        );
        assert_eq!(appdb("mariadb.tables"), Some(1.0));
        assert!(
            point(&points, "mariadb.replication_lag_seconds").is_none()
                && point(&points, "mariadb.replica_sql_running").is_none(),
            "a standalone server is not a replica"
        );
        let _ = setup.close().await;

        // Root rejected → the application user still yields a batch. The
        // replication status and process list it may not read are skipped,
        // not fatal, and not under-reported.
        let bad_root = CollectorConfig::new(
            78,
            SourceKind::Database,
            format!("mysql://root:wrong-password@{host}:{port}/"),
        )
        .with_timeout(Duration::from_secs(10));
        let points = MariaDbCollector::new()
            .with_fallback_connection_string(app_url)
            .collect(&bad_root)
            .await
            .expect("collect never errors");
        assert!(
            point(&points, "mariadb.queries_total").is_some(),
            "fallback to the application user must still collect"
        );
        assert!(
            point(&points, "mariadb.connections_active").is_none(),
            "without PROCESS the process list shows only the user's own threads"
        );
        assert!(points.iter().any(|p| {
            p.name == "mariadb.database_size_bytes"
                && p.labels.get("schema").map(String::as_str) == Some("appdb")
        }));
    }
}
