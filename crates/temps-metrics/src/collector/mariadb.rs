// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! MariaDB metric collector.
//!
//! Opens one short-lived `MySqlConnection` per scrape (no pool, no exporter
//! sidecar) and reads everything over plain SQL:
//!
//! 1. `SHOW GLOBAL STATUS` — connections, running threads, queries, slow
//!    queries, aborted connections, InnoDB buffer pool and row operations,
//!    uptime. Only the variables this collector uses are requested.
//! 2. `SHOW GLOBAL VARIABLES` — `max_connections`.
//! 3. `information_schema.TABLES` — on-disk size (data + indexes) per schema.
//!    Labels are per schema, never per table, and capped at
//!    [`MAX_SCHEMA_SERIES`] so a server with many schemas cannot blow up
//!    label cardinality.
//! 4. `SHOW REPLICA STATUS` (falling back to `SHOW SLAVE STATUS`) — replication
//!    lag, only when the server is a replica.
//!
//! The global status read is the only mandatory step. Every other step is
//! skipped on its own when it fails — typically a missing privilege, e.g. the
//! application user may not run `SHOW REPLICA STATUS` — so one denied query
//! never empties the whole batch.
//!
//! Connection and query failures are logged and produce an empty batch, so a
//! slow or unreachable server never blocks the scraper loop.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlRow};
use sqlx::{ColumnIndex, ConnectOptions, Connection, Executor, Row};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::{debug, warn};

use super::{Collector, CollectorConfig};
use crate::error::MetricsError;
use crate::store::{MetricKind, MetricPoint};

const ENGINE: &str = "mariadb";

/// Upper bound on per-schema `mariadb.database_size_bytes` series. The largest
/// schemas are kept; the unlabelled instance total still covers every schema.
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
/// reflect rows of real tables.
const STATUS_COUNTERS: &[(&str, &str)] = &[
    ("questions", "mariadb.queries_total"),
    ("slow_queries", "mariadb.slow_queries_total"),
    ("aborted_connects", "mariadb.aborted_connects_total"),
    ("aborted_clients", "mariadb.aborted_clients_total"),
    ("handler_write", "mariadb.rows_inserted_total"),
    ("handler_update", "mariadb.rows_updated_total"),
    ("handler_delete", "mariadb.rows_deleted_total"),
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
    ("max_used_connections", "mariadb.max_used_connections"),
    ("uptime", "mariadb.uptime_seconds"),
];

/// Status variables read only to derive the buffer pool hit ratio.
const BUFFER_POOL_READ_REQUESTS: &str = "innodb_buffer_pool_read_requests";
const BUFFER_POOL_READS: &str = "innodb_buffer_pool_reads";

const MAX_CONNECTIONS: &str = "max_connections";

const SCHEMA_SIZES_QUERY: &str = "SELECT TABLE_SCHEMA, \
        CAST(COALESCE(SUM(DATA_LENGTH + INDEX_LENGTH), 0) AS UNSIGNED) AS size_bytes \
     FROM information_schema.TABLES \
     WHERE TABLE_SCHEMA NOT IN ('information_schema', 'performance_schema', 'mysql', 'sys') \
     GROUP BY TABLE_SCHEMA";

/// MariaDB metric collector.
///
/// Stateless apart from the optional fallback credentials: a fresh connection
/// is opened on every [`Collector::collect`] call and closed before it
/// returns.
pub struct MariaDbCollector {
    /// Tried when [`CollectorConfig::connection_string`] cannot log in. The
    /// scraper connects as root first, which sees every schema and the
    /// replication status, and falls back to the application user, which
    /// always works but sees only its own database. An imported server whose
    /// root account only accepts local logins needs the fallback.
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
    /// `(schema, data + index bytes)`; empty when the size query failed.
    pub schema_sizes: Vec<(String, u64)>,
    /// Seconds behind the primary; `None` when this server is not a replica,
    /// replication is stopped, or the status could not be read.
    pub replication_lag_seconds: Option<f64>,
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

    let variables = match conn
        .fetch_all("SHOW GLOBAL VARIABLES WHERE Variable_name = 'max_connections'")
        .await
    {
        Ok(rows) => parse_name_value_rows(rows.iter().filter_map(name_value)),
        Err(e) => {
            warn!(
                source_id,
                engine = ENGINE,
                error = %describe_error(&e),
                "mariadb SHOW GLOBAL VARIABLES failed; skipping max_connections"
            );
            HashMap::new()
        }
    };

    let schema_sizes = match conn.fetch_all(SCHEMA_SIZES_QUERY).await {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                let schema = column_text(row, 0)?;
                let size = column_f64(row, 1)?;
                Some((schema, size.max(0.0) as u64))
            })
            .collect(),
        Err(e) => {
            warn!(
                source_id,
                engine = ENGINE,
                error = %describe_error(&e),
                "mariadb schema size query failed; skipping database sizes"
            );
            Vec::new()
        }
    };

    let replication_lag_seconds = read_replication_lag(conn, source_id).await;

    Some(MariaDbSnapshot {
        status,
        variables,
        schema_sizes,
        replication_lag_seconds,
    })
}

/// `SHOW REPLICA STATUS` exists since MariaDB 10.5.1 and MySQL 8.0.22; older
/// servers only know `SHOW SLAVE STATUS`, which MySQL 8.4 removed. A server
/// that is not a replica returns no row.
async fn read_replication_lag(conn: &mut MySqlConnection, source_id: i32) -> Option<f64> {
    let rows = match conn.fetch_all("SHOW REPLICA STATUS").await {
        Ok(rows) => rows,
        Err(_) => match conn.fetch_all("SHOW SLAVE STATUS").await {
            Ok(rows) => rows,
            Err(e) => {
                // Usually a missing REPLICA MONITOR / REPLICATION CLIENT
                // privilege when connected as the application user.
                debug!(
                    source_id,
                    engine = ENGINE,
                    error = %describe_error(&e),
                    "mariadb replication status unavailable; skipping replication lag"
                );
                return None;
            }
        },
    };
    let row = rows.first()?;
    column_f64(row, "Seconds_Behind_Master").or_else(|| column_f64(row, "Seconds_Behind_Source"))
}

/// `SHOW GLOBAL STATUS` restricted to the variables this collector reads, so
/// the server does not serialise its ~500 status variables every scrape.
fn status_query() -> String {
    let names: Vec<String> = STATUS_COUNTERS
        .iter()
        .chain(STATUS_GAUGES)
        .map(|(variable, _)| *variable)
        .chain(HANDLER_READS.iter().copied())
        .chain([BUFFER_POOL_READ_REQUESTS, BUFFER_POOL_READS])
        .map(|variable| format!("'{variable}'"))
        .collect();
    format!(
        "SHOW GLOBAL STATUS WHERE Variable_name IN ({})",
        names.join(", ")
    )
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

/// Turn a snapshot into metric points. A variable missing from the snapshot
/// skips the metrics that need it; it never fails the batch.
pub(crate) fn build_points(
    snapshot: &MariaDbSnapshot,
    config: &CollectorConfig,
) -> Vec<MetricPoint> {
    let now = Utc::now();
    let mut base_labels: HashMap<String, String> = HashMap::new();
    base_labels.insert("engine".into(), ENGINE.into());
    if let Some(env) = &config.environment {
        base_labels.insert("environment".into(), env.clone());
    }

    let make_point = |name: &str,
                      value: f64,
                      kind: MetricKind,
                      extra_labels: HashMap<String, String>|
     -> MetricPoint {
        let mut labels = base_labels.clone();
        labels.extend(extra_labels);
        MetricPoint {
            time: now,
            source_kind: config.source_kind.clone(),
            source_id: config.source_id,
            name: name.to_owned(),
            value,
            kind,
            engine: Some(ENGINE.into()),
            environment: config.environment.clone(),
            node_id: config.node_id,
            labels,
        }
    };

    let status = |name: &str| snapshot.status.get(name).copied();
    let mut points = Vec::with_capacity(24 + snapshot.schema_sizes.len().min(MAX_SCHEMA_SERIES));

    for (variable, metric) in STATUS_GAUGES {
        if let Some(value) = status(variable) {
            points.push(make_point(metric, value, MetricKind::Gauge, HashMap::new()));
        }
    }
    for (variable, metric) in STATUS_COUNTERS {
        if let Some(value) = status(variable) {
            points.push(make_point(
                metric,
                value,
                MetricKind::Counter,
                HashMap::new(),
            ));
        }
    }

    let reads: Vec<f64> = HANDLER_READS.iter().filter_map(|v| status(v)).collect();
    if !reads.is_empty() {
        points.push(make_point(
            "mariadb.rows_read_total",
            reads.iter().sum(),
            MetricKind::Counter,
            HashMap::new(),
        ));
    }

    // Connection capacity: the headline number to alert on.
    let max_connections = snapshot.variables.get(MAX_CONNECTIONS).copied();
    if let Some(max) = max_connections {
        points.push(make_point(
            "mariadb.max_connections",
            max,
            MetricKind::Gauge,
            HashMap::new(),
        ));
    }
    if let (Some(connected), Some(max)) = (status("threads_connected"), max_connections) {
        if max > 0.0 {
            points.push(make_point(
                "mariadb.connections_used_ratio",
                connected / max,
                MetricKind::Gauge,
                HashMap::new(),
            ));
        }
    }

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
            points.push(make_point(
                "mariadb.buffer_pool_hit_ratio",
                ratio,
                MetricKind::Gauge,
                HashMap::new(),
            ));
        }
    }

    if let Some(lag) = snapshot.replication_lag_seconds {
        points.push(make_point(
            "mariadb.replication_lag_seconds",
            lag,
            MetricKind::Gauge,
            HashMap::new(),
        ));
    }

    // Database size: one series per schema (largest first, capped) plus an
    // unlabelled instance total over every schema for the stat tile.
    if !snapshot.schema_sizes.is_empty() {
        let total: u64 = snapshot
            .schema_sizes
            .iter()
            .fold(0u64, |acc, (_, size)| acc.saturating_add(*size));
        let mut largest: Vec<&(String, u64)> = snapshot.schema_sizes.iter().collect();
        largest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (schema, size) in largest.into_iter().take(MAX_SCHEMA_SERIES) {
            let mut labels = HashMap::new();
            labels.insert("schema".into(), schema.clone());
            points.push(make_point(
                "mariadb.database_size_bytes",
                *size as f64,
                MetricKind::Gauge,
                labels,
            ));
        }
        points.push(make_point(
            "mariadb.database_size_bytes",
            total as f64,
            MetricKind::Gauge,
            HashMap::new(),
        ));
    }

    points
}

#[cfg(test)]
mod tests {
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
            ("Questions", "123456"),
            ("Slow_queries", "7"),
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
        ]))
    }

    fn point<'a>(points: &'a [MetricPoint], name: &str) -> Option<&'a MetricPoint> {
        points
            .iter()
            .find(|p| p.name == name && !p.labels.contains_key("schema"))
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
        for variable in HANDLER_READS {
            assert!(
                query.contains(&format!("'{variable}'")),
                "{variable} missing"
            );
        }
        assert!(query.contains(BUFFER_POOL_READ_REQUESTS));
        assert!(query.contains(BUFFER_POOL_READS));
    }

    #[test]
    fn builds_gauges_counters_and_derived_ratios() {
        let mut variables = HashMap::new();
        variables.insert("max_connections".to_string(), 150.0);
        let snapshot = MariaDbSnapshot {
            status: full_status(),
            variables,
            schema_sizes: vec![],
            replication_lag_seconds: None,
        };

        let points = build_points(&snapshot, &config());

        let value = |name: &str| point(&points, name).map(|p| p.value);
        let kind = |name: &str| point(&points, name).map(|p| p.kind.clone());

        assert_eq!(value("mariadb.connections"), Some(12.0));
        assert_eq!(value("mariadb.max_connections"), Some(150.0));
        assert_eq!(value("mariadb.connections_used_ratio"), Some(12.0 / 150.0));
        assert_eq!(value("mariadb.threads_running"), Some(3.0));
        assert_eq!(value("mariadb.uptime_seconds"), Some(86400.0));
        assert_eq!(value("mariadb.buffer_pool_hit_ratio"), Some(0.99));
        assert_eq!(kind("mariadb.connections"), Some(MetricKind::Gauge));

        for (name, expected) in [
            ("mariadb.queries_total", 123456.0),
            ("mariadb.slow_queries_total", 7.0),
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

        assert!(
            point(&points, "mariadb.replication_lag_seconds").is_none(),
            "a primary reports no replication lag"
        );
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
        let snapshot = MariaDbSnapshot {
            status: full_status(),
            variables: HashMap::from([("max_connections".to_string(), 100.0)]),
            schema_sizes: vec![("app".to_string(), 1)],
            replication_lag_seconds: Some(1.0),
        };
        for p in build_points(&snapshot, &config()) {
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
    fn zero_max_connections_emits_no_ratio() {
        let snapshot = MariaDbSnapshot {
            status: parse_name_value_rows(rows(&[("Threads_connected", "1")])),
            variables: HashMap::from([("max_connections".to_string(), 0.0)]),
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        assert!(point(&points, "mariadb.connections_used_ratio").is_none());
    }

    #[test]
    fn replication_lag_is_reported_for_replicas() {
        let snapshot = MariaDbSnapshot {
            replication_lag_seconds: Some(42.0),
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        assert_eq!(
            point(&points, "mariadb.replication_lag_seconds").map(|p| p.value),
            Some(42.0)
        );
    }

    #[test]
    fn schema_sizes_are_per_schema_plus_an_instance_total() {
        let snapshot = MariaDbSnapshot {
            schema_sizes: vec![("app".to_string(), 3000), ("analytics".to_string(), 1000)],
            ..Default::default()
        };
        let points = build_points(&snapshot, &config());
        let size_of = |schema: Option<&str>| {
            points
                .iter()
                .find(|p| {
                    p.name == "mariadb.database_size_bytes"
                        && p.labels.get("schema").map(String::as_str) == schema
                })
                .map(|p| p.value)
        };
        assert_eq!(size_of(Some("app")), Some(3000.0));
        assert_eq!(size_of(Some("analytics")), Some(1000.0));
        assert_eq!(size_of(None), Some(4000.0));
    }

    #[test]
    fn schema_series_are_capped_but_the_total_covers_every_schema() {
        let schema_sizes: Vec<(String, u64)> = (0..(MAX_SCHEMA_SERIES as u64 + 20))
            .map(|i| (format!("tenant_{i:04}"), i + 1))
            .collect();
        let expected_total: u64 = schema_sizes.iter().map(|(_, s)| s).sum();
        let snapshot = MariaDbSnapshot {
            schema_sizes,
            ..Default::default()
        };

        let points = build_points(&snapshot, &config());
        let labelled: Vec<&MetricPoint> = points
            .iter()
            .filter(|p| p.labels.contains_key("schema"))
            .collect();

        assert_eq!(labelled.len(), MAX_SCHEMA_SERIES);
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
        let _ = setup.close().await;

        let config = CollectorConfig::new(77, SourceKind::Database, root_url)
            .with_timeout(Duration::from_secs(10));
        let points = MariaDbCollector::new()
            .collect(&config)
            .await
            .expect("collect never errors");
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        for expected in [
            "mariadb.connections",
            "mariadb.max_connections",
            "mariadb.connections_used_ratio",
            "mariadb.threads_running",
            "mariadb.uptime_seconds",
            "mariadb.queries_total",
            "mariadb.slow_queries_total",
            "mariadb.aborted_connects_total",
            "mariadb.rows_read_total",
            "mariadb.rows_inserted_total",
            "mariadb.database_size_bytes",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
        let connections = point(&points, "mariadb.connections").map(|p| p.value);
        assert!(
            connections.is_some_and(|c| c >= 1.0),
            "the collector's own connection is counted, got {connections:?}"
        );
        let app_size = points
            .iter()
            .find(|p| {
                p.name == "mariadb.database_size_bytes"
                    && p.labels.get("schema").map(String::as_str) == Some("appdb")
            })
            .map(|p| p.value);
        assert!(
            app_size.is_some_and(|s| s > 0.0),
            "appdb holds an InnoDB table, got {app_size:?}"
        );
        assert!(
            point(&points, "mariadb.replication_lag_seconds").is_none(),
            "a standalone server is not a replica"
        );

        // Root rejected → the application user still yields a batch, and the
        // replication status it may not read is skipped, not fatal.
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
        assert!(points.iter().any(|p| {
            p.name == "mariadb.database_size_bytes"
                && p.labels.get("schema").map(String::as_str) == Some("appdb")
        }));
    }
}
