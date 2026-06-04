//! Database connection management

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use std::sync::Arc;
use std::time::Duration;
use temps_core::{ServiceError, ServiceResult};
use temps_migrations::{Migrator, MigratorTrait};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::debug;

pub type DbConnection = DatabaseConnection;

/// Default timeout for database connectivity check (5 seconds)
const CONNECTIVITY_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Default timeout for database connection establishment (30 seconds)
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Overall timeout for running blocking migrations.
///
/// Raised from the original 120s because schema migrations on large
/// hypertables (e.g. an `ALTER` or backfill on a 20M+ row `proxy_logs`) can
/// legitimately take several minutes. This is the ceiling for migrations that
/// MUST complete before the proxy binds. Heavy, non-correctness-critical work
/// (large index builds) should NOT live in a blocking migration — see
/// `run_post_migration_indexes`, which builds them `CONCURRENTLY` after bind.
const MIGRATION_TIMEOUT: Duration = Duration::from_secs(600);

/// Per-statement lock acquisition timeout applied during migrations.
///
/// A blocking migration that cannot get its lock within this window fails fast
/// (and the service restarts to retry) rather than stalling the entire
/// `MIGRATION_TIMEOUT` budget waiting behind live traffic. This distinguishes
/// "stuck on a contended lock" (fail fast, retry) from "legitimately slow"
/// (holds its own lock, runs to completion).
const MIGRATION_LOCK_TIMEOUT_MS: u64 = 15_000;

/// Parse database URL and extract host and port
fn parse_database_url(database_url: &str) -> Result<(String, u16), String> {
    // Handle postgres:// or postgresql:// URLs
    let url =
        if database_url.starts_with("postgres://") || database_url.starts_with("postgresql://") {
            database_url.to_string()
        } else {
            return Err("Database URL must start with postgres:// or postgresql://".to_string());
        };

    // Parse the URL to extract host and port
    // Format: postgres://user:password@host:port/database
    let without_scheme = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .ok_or("Invalid database URL scheme")?;

    // Find the @ separator (after credentials)
    let host_part = if let Some(at_pos) = without_scheme.rfind('@') {
        &without_scheme[at_pos + 1..]
    } else {
        without_scheme
    };

    // Remove database name (everything after /)
    let host_port = if let Some(slash_pos) = host_part.find('/') {
        &host_part[..slash_pos]
    } else {
        host_part
    };

    // Remove query parameters (everything after ?)
    let host_port = if let Some(query_pos) = host_port.find('?') {
        &host_port[..query_pos]
    } else {
        host_port
    };

    // Parse host and port
    // Handle IPv6 addresses like [::1]:5432
    let (host, port) = if host_port.starts_with('[') {
        // IPv6 address
        if let Some(bracket_end) = host_port.find(']') {
            let ipv6_host = &host_port[1..bracket_end];
            let port_part = &host_port[bracket_end + 1..];
            let port = if let Some(stripped) = port_part.strip_prefix(':') {
                stripped.parse::<u16>().unwrap_or(5432)
            } else {
                5432
            };
            (ipv6_host.to_string(), port)
        } else {
            return Err("Invalid IPv6 address format in database URL".to_string());
        }
    } else if let Some(colon_pos) = host_port.rfind(':') {
        let host = &host_port[..colon_pos];
        let port = host_port[colon_pos + 1..].parse::<u16>().unwrap_or(5432);
        (host.to_string(), port)
    } else {
        (host_port.to_string(), 5432)
    };

    if host.is_empty() {
        return Err("Empty host in database URL".to_string());
    }

    Ok((host, port))
}

/// Check if the database host:port is reachable via TCP
async fn check_database_connectivity(host: &str, port: u16) -> Result<(), String> {
    let addr = format!("{}:{}", host, port);

    match timeout(CONNECTIVITY_CHECK_TIMEOUT, TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("Cannot connect to database at {}: {}", addr, e)),
        Err(_) => Err(format!(
            "Connection to database at {} timed out after {} seconds",
            addr,
            CONNECTIVITY_CHECK_TIMEOUT.as_secs()
        )),
    }
}

pub async fn establish_connection(database_url: &str) -> ServiceResult<Arc<DbConnection>> {
    // Parse the database URL to extract host and port
    let (host, port) = parse_database_url(database_url)
        .map_err(|e| ServiceError::Database(format!("Invalid database URL: {}", e)))?;

    // Check if the database is reachable before attempting to connect
    check_database_connectivity(&host, port)
        .await
        .map_err(ServiceError::Database)?;

    let max_conn: u32 = std::env::var("TEMPS_DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let min_conn: u32 = std::env::var("TEMPS_DB_MIN_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let acquire_timeout_secs: u64 = std::env::var("TEMPS_DB_ACQUIRE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let idle_timeout_secs: u64 = std::env::var("TEMPS_DB_IDLE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);

    let mut opt = ConnectOptions::new(database_url);
    opt.max_connections(max_conn)
        .min_connections(min_conn)
        .connect_timeout(Duration::from_secs(acquire_timeout_secs))
        .idle_timeout(Duration::from_secs(idle_timeout_secs))
        .sqlx_logging(false);

    // Connect with timeout
    let db = match timeout(CONNECTION_TIMEOUT, Database::connect(opt)).await {
        Ok(Ok(db)) => db,
        Ok(Err(e)) => {
            return Err(ServiceError::Database(format!(
                "Failed to connect to database: {}",
                e
            )));
        }
        Err(_) => {
            return Err(ServiceError::Database(format!(
                "Database connection timed out after {} seconds",
                CONNECTION_TIMEOUT.as_secs()
            )));
        }
    };

    // Apply pending migrations. `serve`/`setup` still do this automatically so
    // simple single-node installs keep their zero-step upgrade. The RECOMMENDED
    // enterprise flow is to run `temps migrate` explicitly with the new binary
    // before restarting the server (see docs/upgrade-temps).
    run_migrations(&db).await?;

    // NOTE: continuous-aggregate backfill is intentionally NOT run here. It
    // requires a `CALL refresh_continuous_aggregate()` (a TimescaleDB operation
    // that can be slow) and is not needed to serve traffic. Callers that serve
    // requests (notably `temps serve`) spawn `run_post_migration_backfill` on a
    // long-lived runtime so it never delays startup / the proxy bind. The refresh
    // policy catches up regardless. See `run_post_migration_backfill`.

    Ok(Arc::new(db))
}

/// Connect to the database WITHOUT running migrations.
///
/// Used by the standalone `temps migrate` command (which runs migrations
/// explicitly afterwards) and by any caller that must not trigger schema
/// changes. Performs the same connectivity check and pool configuration as
/// [`establish_connection`].
pub async fn connect_without_migrations(database_url: &str) -> ServiceResult<Arc<DbConnection>> {
    let (host, port) = parse_database_url(database_url)
        .map_err(|e| ServiceError::Database(format!("Invalid database URL: {}", e)))?;

    check_database_connectivity(&host, port)
        .await
        .map_err(ServiceError::Database)?;

    let opt = ConnectOptions::new(database_url);
    let db = match timeout(CONNECTION_TIMEOUT, Database::connect(opt)).await {
        Ok(Ok(db)) => db,
        Ok(Err(e)) => {
            return Err(ServiceError::Database(format!(
                "Failed to connect to database: {}",
                e
            )))
        }
        Err(_) => {
            return Err(ServiceError::Database(format!(
                "Database connection timed out after {} seconds",
                CONNECTION_TIMEOUT.as_secs()
            )))
        }
    };

    Ok(Arc::new(db))
}

/// Apply all pending migrations.
///
/// Uses Sea-ORM's `Migrator::up`, which applies only migrations present in this
/// binary that are NOT yet recorded in `seaql_migrations`. Migration rows in the
/// DB that this binary does not define (e.g. a newer version was run against the
/// DB earlier, or EE-only migrations) are simply ignored — `up` never validates
/// the reverse direction, so an "extra" applied migration can never cause a
/// failure here.
///
/// A short session `lock_timeout` is set first so a migration blocked on a
/// contended lock fails fast (and the operator retries) rather than burning the
/// entire `MIGRATION_TIMEOUT` budget waiting behind live traffic.
pub async fn run_migrations(db: &DbConnection) -> ServiceResult<()> {
    // Fail fast on contended locks rather than hanging the whole budget.
    // Best-effort — non-fatal on setups that reject it.
    if let Err(e) = db
        .execute(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SET lock_timeout = '{}ms'", MIGRATION_LOCK_TIMEOUT_MS),
        ))
        .await
    {
        debug!(
            "Could not set lock_timeout for migrations (non-fatal): {}",
            e
        );
    }

    match timeout(MIGRATION_TIMEOUT, Migrator::up(db, None)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ServiceError::Database(format!(
            "Failed to run migrations: {}",
            e
        ))),
        Err(_) => Err(ServiceError::Database(format!(
            "Database migrations timed out after {} seconds. For large databases, \
             run `temps migrate` manually with the new binary before restarting the server.",
            MIGRATION_TIMEOUT.as_secs()
        ))),
    }
}

/// Outcome of one migration applied by [`run_migrations_reported`].
#[derive(Debug, Clone)]
pub struct MigrationStepResult {
    /// The migration's name (e.g. `m20260603_000001_create_otel_trace_summaries`).
    pub name: String,
    /// Whether `up()` succeeded for this migration.
    pub success: bool,
    /// Wall-clock time the migration took.
    pub elapsed: Duration,
    /// The error message when `success` is false.
    pub error: Option<String>,
}

/// Report returned by [`run_migrations_reported`]: the plan (pending names
/// discovered before applying) and the per-migration results.
#[derive(Debug, Clone, Default)]
pub struct MigrationRunReport {
    /// Names of the migrations that were pending at the start, in apply order.
    pub planned: Vec<String>,
    /// One entry per migration actually attempted, in apply order. On the
    /// first failure, application stops and no further entries are added — so
    /// `results.len() <= planned.len()` and the last entry is the failure.
    pub results: Vec<MigrationStepResult>,
}

impl MigrationRunReport {
    /// True when every planned migration applied successfully (or there was
    /// nothing to apply).
    pub fn all_succeeded(&self) -> bool {
        self.results.iter().all(|r| r.success) && self.results.len() == self.planned.len()
    }

    /// The migration that failed, if any.
    pub fn failed(&self) -> Option<&MigrationStepResult> {
        self.results.iter().find(|r| !r.success)
    }
}

/// Progress event emitted by [`run_migrations_streaming`] as it works through
/// the pending list, so a CLI can give live feedback instead of waiting for the
/// whole run to finish.
#[derive(Debug, Clone)]
pub enum MigrationProgress<'a> {
    /// A migration is about to be applied. `index` is 1-based; `total` is the
    /// number of pending migrations. Emitted before `Migrator::up` runs so the
    /// operator sees which migration is currently in flight (the one that might
    /// hang on a slow index build).
    Started {
        index: usize,
        total: usize,
        name: &'a str,
    },
    /// A migration finished — successfully or not. Carries the same per-step
    /// result that ends up in [`MigrationRunReport::results`].
    Finished {
        index: usize,
        total: usize,
        result: &'a MigrationStepResult,
    },
}

/// Apply pending schema migrations one at a time, invoking `on_progress` before
/// and after each one so callers can give live feedback as the run proceeds.
///
/// This is the streaming core behind [`run_migrations_reported`]. It reads the
/// pending list via `Migrator::get_pending_migrations`, then applies each with
/// `Migrator::up(db, Some(1))`, timing each and stopping at the first failure.
/// `on_progress` fires with [`MigrationProgress::Started`] right before a
/// migration runs and [`MigrationProgress::Finished`] right after — so a CLI can
/// print "applying X…" / "✓ X (820ms)" lines as they happen instead of waiting
/// for the whole batch (a slow index build no longer looks like a hang).
///
/// The same `lock_timeout` guard is applied so a contended lock fails fast
/// instead of hanging, and each step is bounded by [`MIGRATION_TIMEOUT`]. This
/// does NOT run the post-migration continuous-aggregate backfill — the caller
/// runs [`run_post_migration_backfill`] afterwards.
pub async fn run_migrations_streaming<F>(
    db: &DbConnection,
    mut on_progress: F,
) -> ServiceResult<MigrationRunReport>
where
    F: FnMut(MigrationProgress<'_>),
{
    // Fail fast on contended locks rather than hanging (mirrors run_migrations).
    if let Err(e) = db
        .execute(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SET lock_timeout = '{}ms'", MIGRATION_LOCK_TIMEOUT_MS),
        ))
        .await
    {
        debug!(
            "Could not set lock_timeout for migrations (non-fatal): {}",
            e
        );
    }

    // 1. Read the plan: which migrations are pending, in apply order.
    let pending = Migrator::get_pending_migrations(db)
        .await
        .map_err(|e| ServiceError::Database(format!("Failed to read pending migrations: {}", e)))?;

    let total = pending.len();
    let mut report = MigrationRunReport {
        planned: pending.iter().map(|m| m.name().to_string()).collect(),
        results: Vec::with_capacity(total),
    };

    // 2. Apply each pending migration individually so we can time and report it.
    //    `Migrator::up(db, Some(1))` applies exactly the next pending migration.
    for (i, migration) in pending.iter().enumerate() {
        let index = i + 1;
        let name = migration.name().to_string();

        // Announce the migration BEFORE running it: this is the one that might
        // sit for a while on a large table, so the operator needs to see it now.
        on_progress(MigrationProgress::Started {
            index,
            total,
            name: &name,
        });

        let started = std::time::Instant::now();
        let outcome = match timeout(MIGRATION_TIMEOUT, Migrator::up(db, Some(1))).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("{}", e)),
            Err(_) => Err(format!("timed out after {}s", MIGRATION_TIMEOUT.as_secs())),
        };
        let elapsed = started.elapsed();

        let (success, error) = match outcome {
            Ok(()) => (true, None),
            Err(err) => (false, Some(err)),
        };
        report.results.push(MigrationStepResult {
            name,
            success,
            elapsed,
            error,
        });

        // Safe to unwrap: we just pushed. Emit the finished result.
        let result = report.results.last().expect("just pushed a result");
        on_progress(MigrationProgress::Finished {
            index,
            total,
            result,
        });

        // Stop at the first failure — later migrations may depend on the one
        // that just failed, and Sea-ORM won't have applied them.
        if !success {
            break;
        }
    }

    Ok(report)
}

/// Apply pending schema migrations one at a time, returning a [`MigrationRunReport`]
/// so callers can print a plan up front and a pass/fail line per migration.
///
/// Thin wrapper over [`run_migrations_streaming`] that discards progress events
/// and returns only the final report. Prefer `run_migrations_streaming` when you
/// want live per-migration feedback.
pub async fn run_migrations_reported(db: &DbConnection) -> ServiceResult<MigrationRunReport> {
    run_migrations_streaming(db, |_| {}).await
}

/// Read the list of pending migrations WITHOUT applying any of them.
///
/// Returns the names (in apply order) of migrations defined in this binary that
/// are not yet recorded in `seaql_migrations`. Used by `temps migrate --dry-run`
/// to show the plan and by the interactive `--confirm` gate to preview what
/// would run before the operator commits. This is read-only: it never mutates
/// the schema or the migration ledger.
pub async fn get_pending_migration_names(db: &DbConnection) -> ServiceResult<Vec<String>> {
    let pending = Migrator::get_pending_migrations(db)
        .await
        .map_err(|e| ServiceError::Database(format!("Failed to read pending migrations: {}", e)))?;
    Ok(pending.iter().map(|m| m.name().to_string()).collect())
}

/// Run post-migration backfill for continuous aggregates.
///
/// `CALL refresh_continuous_aggregate()` cannot run inside a transaction block,
/// but Sea-ORM migrations run inside transactions. This function runs the backfill
/// after the migration transaction has been committed.
///
/// This is idempotent — refreshing an already-populated aggregate is a no-op for
/// unchanged data, so it's safe to call on every startup.
///
/// Run this on a long-lived runtime (e.g. via `tokio::spawn`) so it never blocks
/// startup; it is decoupled from `establish_connection` for exactly that reason.
pub async fn run_post_migration_backfill(db: &DatabaseConnection) -> ServiceResult<()> {
    // Check if the events_hourly continuous aggregate exists before attempting backfill
    let check_sql = r#"
        SELECT EXISTS (
            SELECT 1 FROM timescaledb_information.continuous_aggregates
            WHERE view_name = 'events_hourly'
        ) as exists
    "#;

    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            check_sql,
        ))
        .await
        .map_err(|e| {
            ServiceError::Database(format!(
                "Failed to check for events_hourly aggregate: {}",
                e
            ))
        })?;

    if let Some(row) = row {
        let exists: bool = row.try_get("", "exists").unwrap_or(false);
        if exists {
            debug!("Backfilling events_hourly continuous aggregate");
            let backfill_sql =
                "CALL refresh_continuous_aggregate('events_hourly', NULL, NOW() - INTERVAL '1 hour')";
            if let Err(e) = db
                .execute(Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    backfill_sql,
                ))
                .await
            {
                // Log but don't fail startup — the refresh policy will catch up
                tracing::warn!(
                    "Failed to backfill events_hourly aggregate (refresh policy will catch up): {}",
                    e
                );
            } else {
                debug!("events_hourly continuous aggregate backfill complete");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_database_url_basic() {
        let (host, port) = parse_database_url("postgres://user:pass@localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_default_port() {
        let (host, port) = parse_database_url("postgres://user:pass@localhost/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_custom_port() {
        let (host, port) =
            parse_database_url("postgresql://user:pass@db.example.com:5433/mydb").unwrap();
        assert_eq!(host, "db.example.com");
        assert_eq!(port, 5433);
    }

    #[test]
    fn test_parse_database_url_with_query_params() {
        let (host, port) =
            parse_database_url("postgres://user:pass@localhost:5432/db?sslmode=require").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_no_credentials() {
        let (host, port) = parse_database_url("postgres://localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_ipv6() {
        let (host, port) = parse_database_url("postgres://user:pass@[::1]:5432/db").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_ipv6_default_port() {
        let (host, port) = parse_database_url("postgres://user:pass@[::1]/db").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_invalid_scheme() {
        let result = parse_database_url("mysql://user:pass@localhost:3306/db");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_database_url_special_chars_in_password() {
        // Password with @ symbol should still work (using rfind for @)
        let (host, port) = parse_database_url("postgres://user:p%40ss@localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }
}
