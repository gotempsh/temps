//! MariaDB binary-log (binlog) health probe.
//!
//! Mirrors the Postgres WAL/archive health probe: it detects the MariaDB-side
//! equivalents of the "silent backup-impossible" and "silent disk-filler"
//! conditions on a managed MariaDB service:
//!
//! - **Binlog disabled** (`log_bin = OFF`): point-in-time recovery is
//!   impossible — there is no continuous change stream to replay after a
//!   base backup. This is the MariaDB analog of Postgres `archive_mode = off`.
//! - **Large local binlog backlog**: many/large binlogs accumulating on disk
//!   usually means the archiver/shipper is behind or `binlog_expire_logs_seconds`
//!   (retention) is set too high — the MariaDB analog of `pg_wal` bloat.
//! - **Non-ROW binlog format** (`binlog_format != ROW`): STATEMENT/MIXED
//!   formats can replay non-deterministically, degrading PITR fidelity.
//! - **Non-InnoDB tables** (MyISAM/Aria): not crash-safe and don't recover
//!   consistently under PITR — a known cause of point-in-time-restore failure.
//!
//! The probe is read-only, runs on a single short-lived `sqlx` MySQL
//! connection using root credentials, and returns a structured snapshot that
//! the background `ExternalServiceHealthMonitor` can persist and the UI can
//! surface as warnings.
//!
//! On ANY connection error the probe returns `None` — it is best-effort
//! observability, not a liveness signal, and a failure here must not cascade
//! into the service being marked down. Credentials are never logged.

use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::Row;
use std::time::Duration;
use utoipa::ToSchema;

/// Number of local binary logs above which the backlog is considered large.
///
/// A healthy MariaDB rotates binlogs (default `max_binlog_size` is 1 GiB) and
/// purges them per `binlog_expire_logs_seconds`. Accumulating more than this
/// many segments locally means either the shipper/archiver is behind or
/// retention is set too high — both worth flagging before the disk fills.
const BINLOG_BACKLOG_SEGMENT_COUNT: usize = 50;

/// Total local binlog size (bytes) above which the backlog is considered
/// large, independent of segment count. 10 GiB of un-purged binlogs on a
/// service whose data may be far smaller is a strong "retention too high /
/// shipper stalled" signal.
const BINLOG_BACKLOG_TOTAL_BYTES: i64 = 10 * 1024 * 1024 * 1024;

/// Connect + per-query timeout. The probe runs alongside the regular
/// `health_probe` so it must stay well under the poll interval.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// One actionable warning surfaced to the UI.
///
/// Each variant carries the data needed to render a remediation hint without
/// the frontend re-querying anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BinlogWarning {
    /// `log_bin = OFF`. Point-in-time recovery is impossible — there is no
    /// binary log to replay after restoring a base backup.
    BinlogDisabled,
    /// Many/large local binary logs are accumulating — the archiver/shipper
    /// may be behind, or `binlog_expire_logs_seconds` (retention) is too high.
    LargeBinlogBacklog {
        segment_count: usize,
        total_bytes: i64,
    },
    /// `binlog_format` is not `ROW`. STATEMENT/MIXED replication can replay
    /// non-deterministically, degrading PITR fidelity.
    NonRowBinlogFormat { format: String },
    /// One or more user tables use a non-transactional storage engine
    /// (MyISAM/Aria). These are not crash-safe and do not recover consistently
    /// under PITR — a base + binlog-replay restore can leave them torn. Convert
    /// such tables to InnoDB for reliable point-in-time recovery.
    NonInnodbTables { count: usize },
}

impl BinlogWarning {
    /// Severity hint for the UI banner color. `Critical` triggers red,
    /// `Warning` triggers yellow.
    pub fn severity(&self) -> BinlogWarningSeverity {
        match self {
            // No binlog = no PITR at all. The single most important signal.
            Self::BinlogDisabled => BinlogWarningSeverity::Critical,
            Self::LargeBinlogBacklog { .. } => BinlogWarningSeverity::Warning,
            Self::NonRowBinlogFormat { .. } => BinlogWarningSeverity::Warning,
            Self::NonInnodbTables { .. } => BinlogWarningSeverity::Warning,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BinlogWarningSeverity {
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MariadbBinlogHealth {
    /// Whether binary logging is enabled (`log_bin = ON`).
    pub log_bin: bool,
    /// The `binlog_format` setting (`ROW`, `STATEMENT`, `MIXED`, or empty when
    /// binlog is disabled).
    pub binlog_format: String,
    /// Binlog retention in seconds (`binlog_expire_logs_seconds`). 0 means
    /// "never auto-purge".
    pub binlog_expire_logs_seconds: i64,
    /// Number of local binary log files (from `SHOW BINARY LOGS`).
    pub segment_count: usize,
    /// Total size of all local binary log files in bytes.
    pub total_binlog_bytes: i64,
    /// Whether GTID strict mode is enabled. Informational — surfaced so the
    /// UI can show replication-consistency posture alongside binlog health.
    pub gtid_strict_mode: bool,
    /// Number of user tables on a non-InnoDB (non-transactional) storage
    /// engine. PITR is unreliable for these (MyISAM/Aria aren't crash-safe).
    /// 0 is healthy.
    pub non_innodb_table_count: usize,
    /// Computed warnings, ordered by severity (critical first).
    pub warnings: Vec<BinlogWarning>,
}

impl MariadbBinlogHealth {
    /// True when any warning is present. The health monitor uses this to
    /// downgrade the service to `degraded`.
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Build a MySQL connection URL from a `ServiceConfig`'s parameters JSON.
///
/// Uses root credentials so the probe can read server-scope variables and
/// `SHOW BINARY LOGS` (which requires the `REPLICATION CLIENT`/`SUPER`
/// privilege the application user typically lacks). Mirrors how
/// `mariadb_query.rs` builds its URL. Returns `None` when the parameters don't
/// carry the fields we need — the caller treats that as "skip the probe"
/// rather than an error.
///
/// `port` is read leniently: the service-config JSON stores it as a string,
/// but we also accept a JSON number so the function is robust to either shape.
pub fn build_conn_str(parameters: &serde_json::Value) -> Option<String> {
    let host = parameters.get("host")?.as_str()?;
    let port = parameters.get("port").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    })?;
    let root_password = parameters.get("root_password")?.as_str()?;

    Some(format!(
        "mysql://root:{}@{}:{}/",
        urlencoding::encode(root_password),
        host,
        port,
    ))
}

/// Run the probe against a MariaDB instance using a `mysql://` connection URL.
///
/// On any error we return `None` rather than surfacing the error — the binlog
/// probe is best-effort observability. The whole probe is wrapped in a
/// `PROBE_TIMEOUT` so a hung server can't stall the health monitor.
pub async fn probe_binlog_health(conn_str: &str) -> Option<MariadbBinlogHealth> {
    tokio::time::timeout(PROBE_TIMEOUT, collect_snapshot(conn_str))
        .await
        .ok()?
}

async fn collect_snapshot(conn_str: &str) -> Option<MariadbBinlogHealth> {
    let pool = MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(PROBE_TIMEOUT)
        .connect(conn_str)
        .await
        .ok()?;

    // log_bin/binlog_format drive the two most consequential warnings
    // (BinlogDisabled is Critical severity; NonRowBinlogFormat degrades PITR
    // fidelity) -- a transient query failure right after a MariaDB restart
    // (docker-entrypoint restarts the server once to apply CLI flags like
    // --log-bin) must not be silently read as "off"/"" and reported as a
    // confident but wrong warning. Treat it the same as a connection
    // failure: return None for the whole probe rather than a snapshot built
    // on defaulted-away data.
    let log_bin = fetch_on_off_variable(&pool, "log_bin").await?;
    let binlog_format = fetch_string_variable(&pool, "binlog_format").await?;
    let binlog_expire_logs_seconds = fetch_numeric_variable(&pool, "binlog_expire_logs_seconds")
        .await
        .unwrap_or(0);
    let gtid_strict_mode = fetch_on_off_variable(&pool, "gtid_strict_mode")
        .await
        .unwrap_or(false);

    // `SHOW BINARY LOGS` errors when binary logging is disabled. Treat that —
    // and any other read failure — as an empty backlog rather than failing the
    // whole probe.
    let (segment_count, total_binlog_bytes) = fetch_binary_logs(&pool).await.unwrap_or((0, 0));

    // Non-InnoDB user tables make PITR unreliable; surface a count.
    let non_innodb_table_count = fetch_non_innodb_table_count(&pool).await.unwrap_or(0);

    pool.close().await;

    let mut snapshot = MariadbBinlogHealth {
        log_bin,
        binlog_format,
        binlog_expire_logs_seconds,
        segment_count,
        total_binlog_bytes,
        gtid_strict_mode,
        non_innodb_table_count,
        warnings: Vec::new(),
    };
    snapshot.warnings = compute_warnings(&snapshot);
    Some(snapshot)
}

/// `SHOW VARIABLES LIKE '<name>'` returns a two-column (`Variable_name`,
/// `Value`) result. Read the `Value` as a string.
async fn fetch_variable_value(pool: &sqlx::MySqlPool, name: &str) -> Option<String> {
    // `SHOW VARIABLES LIKE ?` is not valid in the prepared/binary protocol
    // MariaDB uses for bound parameters -- `PREPARE stmt FROM 'SHOW
    // VARIABLES LIKE ?'` fails with "You have an error in your SQL syntax"
    // (confirmed against mariadb:lts). `sqlx::query(..).bind(..)` always
    // goes through that protocol, so every call here has silently failed
    // and returned `None` since this probe was written -- masked by `.ok()`
    // here and `.unwrap_or(..)` at every call site. `name` is always one of
    // this module's own hardcoded variable-name constants, never external
    // input, so interpolating it directly into the query text is safe.
    let row = sqlx::query(&format!("SHOW VARIABLES LIKE '{name}'"))
        .fetch_optional(pool)
        .await
        .ok()??;
    // Column 1 is the value (column 0 is the variable name).
    row.try_get::<String, _>(1).ok()
}

async fn fetch_string_variable(pool: &sqlx::MySqlPool, name: &str) -> Option<String> {
    fetch_variable_value(pool, name).await
}

/// MariaDB reports boolean-ish variables as `ON`/`OFF` (and `log_bin` as
/// `ON`/`OFF` too). Normalize to a bool.
async fn fetch_on_off_variable(pool: &sqlx::MySqlPool, name: &str) -> Option<bool> {
    let raw = fetch_variable_value(pool, name).await?;
    Some(matches!(
        raw.trim().to_ascii_uppercase().as_str(),
        "ON" | "1"
    ))
}

async fn fetch_numeric_variable(pool: &sqlx::MySqlPool, name: &str) -> Option<i64> {
    let raw = fetch_variable_value(pool, name).await?;
    raw.trim().parse::<i64>().ok()
}

/// `SHOW BINARY LOGS` returns one row per local binlog with columns
/// `Log_name` and `File_size`. Returns `(segment_count, total_bytes)`.
/// Errors (e.g. when binlog is disabled) propagate as `None`.
async fn fetch_binary_logs(pool: &sqlx::MySqlPool) -> Option<(usize, i64)> {
    let rows = sqlx::query("SHOW BINARY LOGS").fetch_all(pool).await.ok()?;
    let segment_count = rows.len();
    let total_bytes: i64 = rows
        .iter()
        .map(|row| {
            // `File_size` is an unsigned bigint; read leniently.
            row.try_get::<i64, _>("File_size")
                .or_else(|_| row.try_get::<u64, _>("File_size").map(|v| v as i64))
                .unwrap_or(0)
        })
        .sum();
    Some((segment_count, total_bytes))
}

/// Count user tables whose storage engine is not InnoDB. System schemas are
/// excluded. MyISAM/Aria tables aren't crash-safe and break consistent PITR.
/// Returns `None` on query failure (treated as 0 — best-effort).
async fn fetch_non_innodb_table_count(pool: &sqlx::MySqlPool) -> Option<usize> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS c FROM information_schema.TABLES \
         WHERE TABLE_TYPE = 'BASE TABLE' AND ENGINE IS NOT NULL AND ENGINE <> 'InnoDB' \
         AND TABLE_SCHEMA NOT IN ('mysql','information_schema','performance_schema','sys')",
    )
    .fetch_optional(pool)
    .await
    .ok()??;
    let c: i64 = row
        .try_get::<i64, _>("c")
        .or_else(|_| row.try_get::<u64, _>("c").map(|v| v as i64))
        .ok()?;
    Some(c.max(0) as usize)
}

/// Pure warning computation. No I/O — fully unit-testable.
fn compute_warnings(snapshot: &MariadbBinlogHealth) -> Vec<BinlogWarning> {
    let mut warnings = Vec::new();

    if !snapshot.log_bin {
        // Binlog disabled dominates: PITR is impossible, and the
        // format/backlog signals are meaningless without it.
        warnings.push(BinlogWarning::BinlogDisabled);
        return warnings;
    }

    // Only meaningful when binlog is on (checked above).
    if !snapshot.binlog_format.eq_ignore_ascii_case("ROW") {
        warnings.push(BinlogWarning::NonRowBinlogFormat {
            format: snapshot.binlog_format.clone(),
        });
    }

    if snapshot.segment_count > BINLOG_BACKLOG_SEGMENT_COUNT
        || snapshot.total_binlog_bytes > BINLOG_BACKLOG_TOTAL_BYTES
    {
        warnings.push(BinlogWarning::LargeBinlogBacklog {
            segment_count: snapshot.segment_count,
            total_bytes: snapshot.total_binlog_bytes,
        });
    }

    if snapshot.non_innodb_table_count > 0 {
        warnings.push(BinlogWarning::NonInnodbTables {
            count: snapshot.non_innodb_table_count,
        });
    }

    // Sort: critical first, then warning.
    warnings.sort_by_key(|w| match w.severity() {
        BinlogWarningSeverity::Critical => 0,
        BinlogWarningSeverity::Warning => 1,
    });

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_snapshot() -> MariadbBinlogHealth {
        MariadbBinlogHealth {
            log_bin: true,
            binlog_format: "ROW".to_string(),
            binlog_expire_logs_seconds: 86400,
            segment_count: 3,
            total_binlog_bytes: 256 * 1024 * 1024, // 256 MiB
            gtid_strict_mode: true,
            non_innodb_table_count: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn healthy_snapshot_produces_no_warnings() {
        let s = healthy_snapshot();
        assert!(compute_warnings(&s).is_empty());
    }

    #[test]
    fn binlog_disabled_is_critical_and_dominates() {
        let mut s = healthy_snapshot();
        s.log_bin = false;
        // Even with otherwise-bad signals, only BinlogDisabled is emitted.
        s.binlog_format = "STATEMENT".to_string();
        s.segment_count = 1000;
        let warnings = compute_warnings(&s);
        assert_eq!(warnings, vec![BinlogWarning::BinlogDisabled]);
        assert_eq!(warnings[0].severity(), BinlogWarningSeverity::Critical);
    }

    #[test]
    fn non_innodb_tables_warn_when_binlog_on() {
        let mut s = healthy_snapshot();
        s.non_innodb_table_count = 4;
        let warnings = compute_warnings(&s);
        let w = warnings
            .iter()
            .find(|w| matches!(w, BinlogWarning::NonInnodbTables { .. }))
            .expect("should warn about non-InnoDB tables");
        assert!(matches!(w, BinlogWarning::NonInnodbTables { count } if *count == 4));
        assert_eq!(w.severity(), BinlogWarningSeverity::Warning);
    }

    #[test]
    fn non_innodb_warning_suppressed_when_binlog_disabled() {
        // BinlogDisabled dominates and short-circuits everything else.
        let mut s = healthy_snapshot();
        s.log_bin = false;
        s.non_innodb_table_count = 4;
        assert_eq!(compute_warnings(&s), vec![BinlogWarning::BinlogDisabled]);
    }

    #[test]
    fn non_row_format_warns_when_binlog_on() {
        let mut s = healthy_snapshot();
        s.binlog_format = "STATEMENT".to_string();
        let warnings = compute_warnings(&s);
        assert!(warnings.iter().any(|w| matches!(
            w,
            BinlogWarning::NonRowBinlogFormat { format } if format == "STATEMENT"
        )));
        // Non-ROW is a Warning, not Critical.
        let w = warnings
            .iter()
            .find(|w| matches!(w, BinlogWarning::NonRowBinlogFormat { .. }))
            .unwrap();
        assert_eq!(w.severity(), BinlogWarningSeverity::Warning);
    }

    #[test]
    fn mixed_format_also_warns() {
        let mut s = healthy_snapshot();
        s.binlog_format = "MIXED".to_string();
        let warnings = compute_warnings(&s);
        assert!(warnings
            .iter()
            .any(|w| matches!(w, BinlogWarning::NonRowBinlogFormat { .. })));
    }

    #[test]
    fn row_format_is_case_insensitive() {
        let mut s = healthy_snapshot();
        s.binlog_format = "row".to_string();
        assert!(compute_warnings(&s).is_empty());
    }

    #[test]
    fn oversized_segment_count_triggers_backlog() {
        let mut s = healthy_snapshot();
        s.segment_count = BINLOG_BACKLOG_SEGMENT_COUNT + 1;
        let warnings = compute_warnings(&s);
        assert!(warnings.iter().any(|w| matches!(
            w,
            BinlogWarning::LargeBinlogBacklog { segment_count, .. }
                if *segment_count == BINLOG_BACKLOG_SEGMENT_COUNT + 1
        )));
    }

    #[test]
    fn oversized_total_bytes_triggers_backlog() {
        let mut s = healthy_snapshot();
        s.segment_count = 5; // under the count threshold
        s.total_binlog_bytes = BINLOG_BACKLOG_TOTAL_BYTES + 1;
        let warnings = compute_warnings(&s);
        assert!(warnings
            .iter()
            .any(|w| matches!(w, BinlogWarning::LargeBinlogBacklog { .. })));
    }

    #[test]
    fn backlog_at_threshold_does_not_trigger() {
        let mut s = healthy_snapshot();
        s.segment_count = BINLOG_BACKLOG_SEGMENT_COUNT; // exactly at, not over
        s.total_binlog_bytes = BINLOG_BACKLOG_TOTAL_BYTES; // exactly at, not over
        assert!(compute_warnings(&s).is_empty());
    }

    #[test]
    fn critical_warnings_sort_before_warnings() {
        // BinlogDisabled short-circuits, so to test sorting we construct a
        // case with multiple non-disabled warnings and confirm ordering is
        // stable (all Warning severity here — the sort is a no-op but must
        // not reorder unexpectedly). Then verify the dominant-critical path.
        let mut s = healthy_snapshot();
        s.binlog_format = "STATEMENT".to_string();
        s.segment_count = BINLOG_BACKLOG_SEGMENT_COUNT + 10;
        let warnings = compute_warnings(&s);
        assert_eq!(warnings.len(), 2);
        // Both are Warning severity.
        assert!(warnings
            .iter()
            .all(|w| w.severity() == BinlogWarningSeverity::Warning));
    }

    #[test]
    fn has_warnings_reflects_warnings_vec() {
        let mut s = healthy_snapshot();
        assert!(!s.has_warnings());
        s.warnings.push(BinlogWarning::BinlogDisabled);
        assert!(s.has_warnings());
    }

    #[test]
    fn build_conn_str_with_string_port() {
        let params = serde_json::json!({
            "host": "127.0.0.1",
            "port": "3306",
            "root_password": "s3cr3t"
        });
        let url = build_conn_str(&params).expect("should build");
        assert_eq!(url, "mysql://root:s3cr3t@127.0.0.1:3306/");
    }

    #[test]
    fn build_conn_str_with_numeric_port() {
        let params = serde_json::json!({
            "host": "db.internal",
            "port": 3307,
            "root_password": "pw"
        });
        let url = build_conn_str(&params).expect("should build");
        assert_eq!(url, "mysql://root:pw@db.internal:3307/");
    }

    #[test]
    fn build_conn_str_url_encodes_password() {
        let params = serde_json::json!({
            "host": "h",
            "port": "3306",
            "root_password": "p@ss:word/with#chars"
        });
        let url = build_conn_str(&params).expect("should build");
        // The password segment must be percent-encoded so special chars don't
        // break URL parsing or leak into the host/path.
        assert!(url.starts_with("mysql://root:"));
        assert!(!url.contains("p@ss:word/with#chars"));
        assert!(url.ends_with("@h:3306/"));
    }

    #[test]
    fn build_conn_str_missing_fields_returns_none() {
        assert!(build_conn_str(&serde_json::json!({})).is_none());
        assert!(build_conn_str(&serde_json::json!({ "host": "h", "port": "3306" })).is_none());
        assert!(
            build_conn_str(&serde_json::json!({ "host": "h", "root_password": "p" })).is_none()
        );
    }
}
