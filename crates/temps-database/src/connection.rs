// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database connection management

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use sqlx::Acquire;
use std::sync::Arc;
use std::time::Duration;
use temps_core::{ServiceError, ServiceResult};
use temps_migrations::{Migrator, MigratorTrait};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::debug;

pub type DbConnection = DatabaseConnection;

/// Statements slower than this are reported at WARN (`sqlx::query` target).
const SLOW_STATEMENT_THRESHOLD: Duration = Duration::from_secs(1);

/// Sea-ORM connect options carrying Temps' SQL logging policy.
///
/// Every statement is logged at TRACE, so per-query SQL only appears when
/// explicitly requested (`RUST_LOG=sqlx::query=trace`) and never floods a
/// `RUST_LOG=info`/`debug` run. Sea-ORM's default logs every statement at
/// INFO. Statements slower than [`SLOW_STATEMENT_THRESHOLD`] are still
/// reported at WARN: `sqlx_logging(false)` would silence those too, and they
/// are the only signal that a query is degrading.
///
/// Use this for every Postgres connection instead of `ConnectOptions::new`.
pub fn connect_options(database_url: impl Into<String>) -> ConnectOptions {
    let mut opt = ConnectOptions::new(database_url);
    opt.sqlx_logging(true)
        .sqlx_logging_level(log::LevelFilter::Trace)
        .sqlx_slow_statements_logging_settings(log::LevelFilter::Warn, SLOW_STATEMENT_THRESHOLD);
    opt
}

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

/// Default for how long `temps serve` waits for its database at startup.
///
/// Long enough to ride out a database container that starts alongside the
/// server, or a short network outage, without a supervisor turning it into a
/// restart loop; short enough that a wrong host is reported within minutes.
pub const DEFAULT_DATABASE_STARTUP_WAIT: Duration = Duration::from_secs(120);

/// Delay before the second startup attempt; doubled after every failure.
const STARTUP_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(1);

/// Ceiling for the delay between two startup attempts.
const STARTUP_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// Why one attempt to reach the database at startup failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAttemptFailure {
    /// The database may still come up: nothing listens yet, the host is
    /// unreachable or does not resolve yet, or PostgreSQL is starting up.
    Transient(String),
    /// Waiting cannot help: wrong credentials, a missing database, an invalid
    /// URL. Reported at once instead of after the whole wait.
    Permanent(String),
}

/// Delay after the `failed_attempts`-th failed attempt (1-based): 1 s, 2 s,
/// 4 s, ... capped at [`STARTUP_RETRY_MAX_DELAY`].
fn startup_retry_delay(failed_attempts: u32) -> Duration {
    let exponent = failed_attempts.saturating_sub(1).min(16);
    STARTUP_RETRY_INITIAL_DELAY
        .saturating_mul(1u32 << exponent)
        .min(STARTUP_RETRY_MAX_DELAY)
}

/// Run `attempt` until it succeeds, it fails permanently, or `max_wait` has
/// elapsed, sleeping with exponential backoff between transient failures.
///
/// Every failed attempt is logged with its reason and the next delay. The
/// final error names the target, the number of attempts, how long it waited,
/// the last reason, and the setting that raises the limit. A `max_wait` of
/// zero makes exactly one attempt.
pub async fn retry_database_startup<T, F, Fut>(
    target: &str,
    max_wait: Duration,
    mut attempt: F,
) -> ServiceResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, StartupAttemptFailure>>,
{
    let started = tokio::time::Instant::now();
    let give_up = |attempts: u32, reason: &str| {
        ServiceError::Database(format!(
            "Database at {target} is still unreachable after waiting {}s \
             ({attempts} attempt(s)); last error: {reason}. Check that PostgreSQL \
             is running and reachable from this host, or allow a longer wait with \
             --database-startup-wait / TEMPS_DB_STARTUP_WAIT (seconds)",
            started.elapsed().as_secs()
        ))
    };
    let mut failed_attempts: u32 = 0;
    loop {
        // Each attempt gets only the time left, so a slow probe (a TCP check
        // to a black-holed host, a login that hangs) cannot carry startup
        // past the limit. A zero wait makes one attempt with its own
        // timeouts, as before.
        let outcome = if max_wait.is_zero() {
            attempt().await
        } else {
            let remaining = max_wait.saturating_sub(started.elapsed());
            match tokio::time::timeout(remaining, attempt()).await {
                Ok(outcome) => outcome,
                Err(_) => Err(StartupAttemptFailure::Transient(format!(
                    "the attempt did not finish within the {}s left of the startup wait",
                    remaining.as_secs()
                ))),
            }
        };
        let reason = match outcome {
            Ok(value) => {
                if failed_attempts > 0 {
                    tracing::info!(
                        target_database = %target,
                        attempts = failed_attempts + 1,
                        waited_secs = started.elapsed().as_secs(),
                        "Database is reachable after waiting at startup"
                    );
                }
                return Ok(value);
            }
            Err(StartupAttemptFailure::Permanent(reason)) => {
                return Err(ServiceError::Database(format!(
                    "Cannot use the database at {target}: {reason}. Waiting would not \
                     help, so startup stops here; fix the database URL, credentials or \
                     database name"
                )));
            }
            Err(StartupAttemptFailure::Transient(reason)) => reason,
        };
        failed_attempts += 1;
        let elapsed = started.elapsed();
        let remaining = max_wait.saturating_sub(elapsed);
        if remaining.is_zero() {
            return Err(give_up(failed_attempts, &reason));
        }
        let delay = startup_retry_delay(failed_attempts);
        if delay >= remaining {
            // The next attempt would start with no time left to run in.
            tokio::time::sleep(remaining).await;
            return Err(give_up(failed_attempts, &reason));
        }
        tracing::warn!(
            target_database = %target,
            attempt = failed_attempts,
            waited_secs = elapsed.as_secs(),
            max_wait_secs = max_wait.as_secs(),
            retry_in_ms = delay.as_millis() as u64,
            reason = %reason,
            "Database not reachable yet at startup; retrying"
        );
        tokio::time::sleep(delay).await;
    }
}

/// Whether a failed connection attempt is worth retrying at startup.
///
/// Transport failures and the two server answers that mean "not yet"
/// (`57P03` the database system is starting up, `53300` too many
/// connections) are transient; any other answer from PostgreSQL (bad
/// password, unknown database, permission denied) is permanent.
fn classify_connect_error(error: &sea_orm::DbErr) -> StartupAttemptFailure {
    use sea_orm::{DbErr, RuntimeErr};
    let reason = error.to_string();
    let sqlx_error = match error {
        DbErr::Conn(RuntimeErr::SqlxError(sqlx_error)) => sqlx_error,
        _ => return StartupAttemptFailure::Transient(reason),
    };
    match sqlx_error {
        sqlx::Error::Database(database_error) => match database_error.code().as_deref() {
            Some("57P03") | Some("53300") => StartupAttemptFailure::Transient(reason),
            _ => StartupAttemptFailure::Permanent(reason),
        },
        sqlx::Error::Configuration(_) => StartupAttemptFailure::Permanent(reason),
        _ => StartupAttemptFailure::Transient(reason),
    }
}

/// One startup probe: the TCP check, then a real login with a single
/// throwaway connection, so a PostgreSQL that accepts TCP but is still
/// starting up counts as not ready yet.
async fn probe_database_once(
    database_url: &str,
    host: &str,
    port: u16,
) -> Result<(), StartupAttemptFailure> {
    check_database_connectivity(host, port)
        .await
        .map_err(StartupAttemptFailure::Transient)?;
    let mut options = connect_options(database_url);
    options
        .max_connections(1)
        .min_connections(0)
        .connect_timeout(CONNECTIVITY_CHECK_TIMEOUT);
    let probe = match timeout(CONNECTION_TIMEOUT, Database::connect(options)).await {
        Ok(Ok(probe)) => probe,
        Ok(Err(error)) => return Err(classify_connect_error(&error)),
        Err(_) => {
            return Err(StartupAttemptFailure::Transient(format!(
                "login timed out after {} seconds",
                CONNECTION_TIMEOUT.as_secs()
            )))
        }
    };
    let _ = probe.close().await;
    Ok(())
}

/// Wait for the database at startup instead of exiting on the first failure.
///
/// `temps serve` calls this before it opens its pool. A database that comes
/// up within `max_wait` (container started alongside the server, short
/// outage) no longer turns a supervisor's restart policy into a restart loop.
/// Each attempt is logged with its reason; the backoff starts at 1 s and
/// doubles up to 30 s. Wrong credentials or a missing database fail at once.
pub async fn wait_for_database(database_url: &str, max_wait: Duration) -> ServiceResult<()> {
    let (host, port) = parse_database_url(database_url)
        .map_err(|e| ServiceError::Database(format!("Invalid database URL: {}", e)))?;
    let target = format!("{host}:{port}");
    retry_database_startup(&target, max_wait, || {
        probe_database_once(database_url, &host, port)
    })
    .await
}

/// Refuse an existing schema upgrade from a caller without backup preparation.
/// Fresh databases may still be initialized without a rollback snapshot.
pub async fn ensure_no_unprotected_upgrade(db: &DbConnection) -> ServiceResult<()> {
    let status = crate::check_schema_compatibility(db)
        .await
        .map_err(|error| ServiceError::Database(error.to_string()))?;
    if !status.pending().is_empty() {
        return Err(ServiceError::Database(format!(
            "Refusing {} pending migration(s) without a pre-migration backup. \
             Run `temps migrate` or `temps serve` with this binary first.",
            status.pending().len()
        )));
    }
    Ok(())
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

    let mut opt = connect_options(database_url);
    opt.max_connections(max_conn)
        .min_connections(min_conn)
        .connect_timeout(Duration::from_secs(acquire_timeout_secs))
        .idle_timeout(Duration::from_secs(idle_timeout_secs));

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

    // Utility commands and the standalone proxy cannot take a rollback backup.
    // Initialize fresh installs, but require the protected CLI upgrade path
    // before changing an existing database.
    ensure_no_unprotected_upgrade(&db).await?;
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

    let opt = connect_options(database_url);
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

/// Connect for the standalone `temps migrate` command: a SINGLE persistent
/// backend (pool of 1), tagged `application_name = 'temps_migrate'`, returning
/// that backend's PID alongside the connection.
///
/// The single, stable backend is what makes interruption safe. If the operator
/// hits Ctrl+C during a slow DDL (e.g. building an index), the CLI process exits
/// but the Postgres backend keeps running the statement server-side — and a
/// re-run of `temps migrate` would then race a SECOND backend against the first
/// on the same schema. Capturing the PID lets the command cancel that exact
/// backend on interrupt (see [`cancel_migration_backend`]).
pub async fn connect_for_migrate(database_url: &str) -> ServiceResult<(Arc<DbConnection>, i32)> {
    let (host, port) = parse_database_url(database_url)
        .map_err(|e| ServiceError::Database(format!("Invalid database URL: {}", e)))?;

    check_database_connectivity(&host, port)
        .await
        .map_err(ServiceError::Database)?;

    let mut opt = connect_options(database_url);
    // Exactly one backend, kept alive for the run, so the PID below is the
    // backend that executes every migration statement.
    opt.max_connections(1).min_connections(1);

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

    // Tag the session so operators can spot the migrator in `pg_stat_activity`
    // and an interrupt can find it by name as a fallback. Best-effort.
    let _ = db
        .execute(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SET application_name = 'temps_migrate'".to_owned(),
        ))
        .await;

    let pid = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT pg_backend_pid() AS pid".to_owned(),
        ))
        .await
        .map_err(|e| ServiceError::Database(format!("Failed to read backend pid: {}", e)))?
        .and_then(|row| row.try_get::<i32>("", "pid").ok())
        .ok_or_else(|| ServiceError::Database("Could not determine backend pid".to_owned()))?;

    Ok((Arc::new(db), pid))
}

/// Whether `pid` is still a live backend executing a query.
///
/// A PID that has disconnected entirely is absent from `pg_stat_activity` and
/// therefore reported as not active — the caller must be able to distinguish
/// "gone" (safe) from "still running the DDL" (unsafe to re-run).
async fn migration_backend_active(db: &DatabaseConnection, pid: i32) -> ServiceResult<bool> {
    db.query_one(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT EXISTS (\
             SELECT 1 FROM pg_stat_activity WHERE pid = $1 AND state = 'active'\
         ) AS active",
        [pid.into()],
    ))
    .await
    .map_err(|error| {
        ServiceError::Database(format!(
            "Failed to verify cancellation of migration backend {pid}: {error}"
        ))
    })?
    .ok_or_else(|| {
        ServiceError::Database(format!(
            "PostgreSQL returned no verification result for migration backend {pid}"
        ))
    })?
    .try_get::<bool>("", "active")
    .map_err(|error| {
        ServiceError::Database(format!(
            "Failed to decode cancellation verification for migration backend {pid}: {error}"
        ))
    })
}

/// Cancel an in-flight migration server-side — used when `temps migrate` is
/// interrupted (Ctrl+C). Opens a FRESH connection (the migrating one is busy)
/// and sends a query-cancel to the captured backend `pid`, exactly like pressing
/// Ctrl+C in `psql`: the running DDL aborts and, because Sea-ORM runs each
/// migration in a transaction, that transaction rolls back — leaving the DB at
/// the last successfully-applied migration. Cancellation is scoped to the exact
/// captured PID; another legitimate `temps migrate` process is never swept by
/// application name.
pub async fn cancel_migration_backend(database_url: &str, pid: i32) -> ServiceResult<()> {
    let db = Database::connect(connect_options(database_url))
        .await
        .map_err(|e| {
            ServiceError::Database(format!("Failed to connect to cancel migration: {}", e))
        })?;

    let cancellation = db
        .query_one(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT pg_cancel_backend($1) AS cancelled",
            [pid.into()],
        ))
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to request cancellation for migration backend {pid}: {error}"
            ))
        })?
        .ok_or_else(|| {
            ServiceError::Database(format!(
                "PostgreSQL returned no cancellation result for migration backend {pid}"
            ))
        })?
        .try_get::<bool>("", "cancelled")
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to decode cancellation result for migration backend {pid}: {error}"
            ))
        })?;
    if !cancellation {
        // `pg_cancel_backend` also returns false — with a
        // "PID N is not a PostgreSQL backend process" warning — when the target
        // is ALREADY GONE. That is the safe outcome, not a failure: there is no
        // statement left to cancel and nothing for a re-run to race. Only treat
        // it as a failure when the backend is still there running something.
        if !migration_backend_active(&db, pid).await? {
            return Ok(());
        }
        return Err(ServiceError::Database(format!(
            "PostgreSQL did not confirm cancellation of migration backend {pid}"
        )));
    }

    // `pg_cancel_backend` confirms signal delivery, not that statement cleanup
    // and transaction rollback have completed. Do not tell the operator it is
    // safe to rerun until the target is no longer executing an active query.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let stopped = !migration_backend_active(&db, pid).await?;
        if stopped {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ServiceError::Database(format!(
                "Migration backend {pid} remained active for 5 seconds after cancellation"
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    Ok(())
}

/// Run the schema guard and surface a refusal as a [`ServiceError`] whose
/// message carries the guard's full explanation and remediation.
async fn ensure_schema_compatible(db: &DbConnection) -> ServiceResult<()> {
    crate::schema_guard::check_schema_compatibility(db)
        .await
        .map(|_| ())
        .map_err(|error| match error {
            crate::schema_guard::SchemaGuardError::DatabaseNewerThanBinary { .. } => {
                ServiceError::Configuration {
                    message: error.to_string(),
                }
            }
            crate::schema_guard::SchemaGuardError::ReadLedger { .. } => {
                ServiceError::Database(error.to_string())
            }
        })
}

/// Apply all pending migrations.
///
/// Uses Sea-ORM's `Migrator::up`, which applies only migrations present in this
/// binary that are NOT yet recorded in `seaql_migrations`. Before that, the
/// schema guard ([`crate::schema_guard::check_schema_compatibility`]) refuses a
/// database that has applied migrations this binary does not define — i.e. one
/// migrated by a newer release — with an error naming them and the
/// remediation, instead of letting an older binary run against a newer schema.
///
/// A short session `lock_timeout` is set first so a migration blocked on a
/// contended lock fails fast (and the operator retries) rather than burning the
/// entire `MIGRATION_TIMEOUT` budget waiting behind live traffic.
pub async fn run_migrations(db: &DbConnection) -> ServiceResult<()> {
    ensure_schema_compatible(db).await?;

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
    // Refuse a database migrated by a newer release before touching anything.
    ensure_schema_compatible(db).await?;

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
    ensure_schema_compatible(db).await?;
    let pending = Migrator::get_pending_migrations(db)
        .await
        .map_err(|e| ServiceError::Database(format!("Failed to read pending migrations: {}", e)))?;
    Ok(pending.iter().map(|m| m.name().to_string()).collect())
}

/// Number of migrations defined in this binary. Compared with the pending
/// count to tell a fresh install (everything pending) from an upgrade (some
/// pending on a database that already has applied migrations).
pub fn defined_migration_count() -> usize {
    Migrator::migrations().len()
}

const PERMISSION_DENIED_RETENTION_INDEX: &str = "idx_audit_logs_permission_denied_retention";

/// An index on a column that references `ip_geolocations` (or on
/// `visitor.last_seen`), built after boot rather than inside a migration.
///
/// Deleting stored IP addresses makes PostgreSQL look for referencing rows,
/// and the retention job asks the same question per candidate, so each
/// referencing column needs an index. On a large install a plain
/// `CREATE INDEX` holds a `SHARE` lock that stops ingestion for the whole
/// build, which is why these are not in a migration (SeaORM wraps migrations
/// in a transaction, where `CONCURRENTLY` is illegal).
struct ReferenceIndex {
    name: &'static str,
    table: &'static str,
    column: &'static str,
    /// Index only rows where the column is set. Most rows of the high-volume
    /// tables carry no address, so a partial index costs them nothing.
    partial: bool,
    /// TimescaleDB does not support `CREATE INDEX CONCURRENTLY` on a
    /// hypertable; it builds one chunk per transaction instead, so a write is
    /// only ever blocked behind a single chunk's build. Must match the
    /// migrations: `error_events` and `proxy_logs` are hypertables
    /// (`m20250101_000001_initial_schema`), the other tables are not.
    hypertable: bool,
}

const REFERENCE_INDEXES: &[ReferenceIndex] = &[
    ReferenceIndex {
        name: "idx_visitor_last_seen",
        table: "visitor",
        column: "last_seen",
        partial: false,
        hypertable: false,
    },
    ReferenceIndex {
        name: "idx_error_events_ip_geolocation",
        table: "error_events",
        column: "ip_geolocation_id",
        partial: true,
        hypertable: true,
    },
    ReferenceIndex {
        name: "idx_proxy_logs_ip_geolocation",
        table: "proxy_logs",
        column: "ip_geolocation_id",
        partial: true,
        hypertable: true,
    },
    ReferenceIndex {
        name: "idx_visitor_ip_address",
        table: "visitor",
        column: "ip_address_id",
        partial: true,
        hypertable: false,
    },
    ReferenceIndex {
        name: "idx_performance_metrics_ip_address",
        table: "performance_metrics",
        column: "ip_address_id",
        partial: true,
        hypertable: false,
    },
    ReferenceIndex {
        name: "idx_audit_logs_ip_address",
        table: "audit_logs",
        column: "ip_address_id",
        partial: true,
        hypertable: false,
    },
];

impl ReferenceIndex {
    fn create_sql(&self) -> String {
        let predicate = if self.partial {
            format!(" WHERE {} IS NOT NULL", self.column)
        } else {
            String::new()
        };
        if self.hypertable {
            // `WITH (...)` precedes `WHERE` in the CREATE INDEX grammar.
            format!(
                "CREATE INDEX IF NOT EXISTS {name} ON {table} ({column}) \
                 WITH (timescaledb.transaction_per_chunk){predicate}",
                name = self.name,
                table = self.table,
                column = self.column,
            )
        } else {
            format!(
                "CREATE INDEX CONCURRENTLY IF NOT EXISTS {name} ON {table} ({column}){predicate}",
                name = self.name,
                table = self.table,
                column = self.column,
            )
        }
    }

    /// Removes the invalid leftover of a cancelled or failed build, which
    /// `IF NOT EXISTS` would otherwise accept forever.
    fn drop_invalid_sql(&self) -> String {
        if self.hypertable {
            format!("DROP INDEX IF EXISTS {}", self.name)
        } else {
            format!("DROP INDEX CONCURRENTLY IF EXISTS {}", self.name)
        }
    }
}

/// Build every [`REFERENCE_INDEXES`] entry whose column exists, on the
/// maintenance connection. Each index is attempted even if an earlier one
/// failed; the failures are reported together so the caller retries the set
/// (every statement is idempotent).
async fn build_reference_indexes<F>(
    connection: &mut sqlx::PgConnection,
    on_progress: &F,
) -> ServiceResult<()>
where
    F: Fn(MaintenanceProgress<'_>),
{
    let mut failures: Vec<String> = Vec::new();
    for index in REFERENCE_INDEXES {
        let column_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = $1 AND column_name = $2)",
        )
        .bind(index.table)
        .bind(index.column)
        .fetch_one(&mut *connection)
        .await;
        match column_exists {
            Ok(true) => {}
            // A table or column that this installation does not have.
            Ok(false) => continue,
            Err(error) => {
                failures.push(format!(
                    "{} (checking {}.{}): {error}",
                    index.name, index.table, index.column
                ));
                continue;
            }
        }

        let invalid = sqlx::query_scalar::<_, bool>(
            "SELECT indisvalid FROM pg_index WHERE indexrelid = to_regclass($1)",
        )
        .bind(index.name)
        .fetch_optional(&mut *connection)
        .await;
        match invalid {
            Ok(Some(false)) => {
                if let Err(error) = sqlx::query(&index.drop_invalid_sql())
                    .execute(&mut *connection)
                    .await
                {
                    failures.push(format!(
                        "{} (removing an invalid earlier build): {error}",
                        index.name
                    ));
                    continue;
                }
            }
            Ok(_) => {}
            Err(error) => {
                failures.push(format!("{} (inspecting it): {error}", index.name));
                continue;
            }
        }

        on_progress(MaintenanceProgress::IndexStarted { name: index.name });
        let started = std::time::Instant::now();
        match sqlx::query(&index.create_sql())
            .execute(&mut *connection)
            .await
        {
            Ok(_) => on_progress(MaintenanceProgress::IndexFinished {
                name: index.name,
                elapsed: started.elapsed(),
            }),
            Err(error) => failures.push(format!(
                "{} on {}({}): {error}",
                index.name, index.table, index.column
            )),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ServiceError::Database(format!(
            "Failed to build {} post-migration reference index(es): {}",
            failures.len(),
            failures.join("; ")
        )))
    }
}

/// Live progress events from post-migration maintenance (concurrent index
/// builds and continuous-aggregate backfill).
///
/// Both phases are non-transactional and can legitimately run for minutes on a
/// large install — a concurrent index build scans the whole audit history, and
/// the backfill issues one `refresh_continuous_aggregate()` transaction per
/// window. Without these events an operator watching `temps migrate` sees a
/// frozen terminal after the last migration line and cannot tell "still
/// working" from "wedged on a lock". Callers turn them into per-step output.
#[derive(Debug, Clone)]
pub enum MaintenanceProgress<'a> {
    /// The PostgreSQL backend PID that will execute the phase about to start.
    ///
    /// Emitted first by each phase. It is NOT necessarily the PID captured by
    /// [`connect_for_migrate`]: the index phase deliberately disposes of its
    /// connection (`close_on_drop`), so the backfill that follows runs on a
    /// freshly-opened backend with a different PID. A caller that cancels
    /// server-side on interrupt must target this PID, not the connect-time one,
    /// or it will signal a backend that no longer exists while the real work
    /// keeps running.
    PhaseBackend { pid: i32 },
    /// A concurrent index build is about to start.
    IndexStarted { name: &'a str },
    /// The concurrent index build finished successfully.
    IndexFinished { name: &'a str, elapsed: Duration },
    /// A continuous aggregate is about to be refreshed over `windows` windows.
    AggregateStarted { view: &'a str, windows: usize },
    /// One refresh window finished. `error` is set when that window failed —
    /// windows are independent, so the backfill continues either way.
    AggregateWindow {
        view: &'a str,
        index: usize,
        total: usize,
        elapsed: Duration,
        error: Option<String>,
    },
    /// Every window for this aggregate has been attempted.
    AggregateFinished {
        view: &'a str,
        windows: usize,
        failed: usize,
        elapsed: Duration,
    },
}

/// Build large, non-correctness-critical indexes outside SeaORM's migration
/// transaction. The caller runs this after the server has bound (or explicitly
/// from `temps migrate`), so a busy audit table cannot block application boot.
///
/// A single acquired pool connection is retained for the timeout settings and
/// the concurrent index build. `CREATE INDEX CONCURRENTLY` allows audit writes
/// to continue while PostgreSQL scans the existing history.
pub async fn run_post_migration_indexes(db: &DatabaseConnection) -> ServiceResult<()> {
    run_post_migration_indexes_streaming(db, |_| {}).await
}

/// [`run_post_migration_indexes`] with live progress events.
///
/// `on_progress` is called immediately before the concurrent build starts and
/// again when it completes, so an interactive caller can show which index is
/// being built rather than an unexplained pause.
pub async fn run_post_migration_indexes_streaming<F>(
    db: &DatabaseConnection,
    on_progress: F,
) -> ServiceResult<()>
where
    F: Fn(MaintenanceProgress<'_>),
{
    let pool = db.get_postgres_connection_pool();
    let mut connection = pool.acquire().await.map_err(|error| {
        ServiceError::Database(format!(
            "Failed to acquire database connection for post-migration indexes: {error}"
        ))
    })?;
    // Session timeouts cannot be reset asynchronously from Drop if this future
    // is cancelled. Treat this as a disposable maintenance connection so no
    // cancellation or RESET failure can leak its settings back into the pool.
    connection.close_on_drop();

    // Report the backend actually doing the work. Because this connection is
    // disposable, it is the LAST phase to run on the connect-time PID — anything
    // after it gets a new backend, and an interrupt aimed at the old PID would
    // signal a process that no longer exists.
    if let Ok(pid) = sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await
    {
        on_progress(MaintenanceProgress::PhaseBackend { pid });
    }

    sqlx::query("SET lock_timeout = '5s'")
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to set lock timeout for permission-denied retention index: {error}"
            ))
        })?;
    if let Err(error) = sqlx::query("SET statement_timeout = '10min'")
        .execute(&mut *connection)
        .await
    {
        let _ = sqlx::query("RESET lock_timeout")
            .execute(&mut *connection)
            .await;
        return Err(ServiceError::Database(format!(
            "Failed to set statement timeout for permission-denied retention index: {error}"
        )));
    }

    let maintenance_result: ServiceResult<()> = async {
        let index_is_valid = sqlx::query_scalar::<_, bool>(
            "SELECT index.indisvalid \
             FROM pg_index AS index \
             WHERE index.indexrelid = \
                 to_regclass('idx_audit_logs_permission_denied_retention')",
        )
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to inspect permission-denied retention index: {error}"
            ))
        })?;

        // A cancelled/failed concurrent build leaves an invalid index behind.
        // `IF NOT EXISTS` would otherwise accept it forever and prevent retries.
        if index_is_valid == Some(false) {
            sqlx::query(
                "DROP INDEX CONCURRENTLY IF EXISTS idx_audit_logs_permission_denied_retention",
            )
            .execute(&mut *connection)
            .await
            .map_err(|error| {
                ServiceError::Database(format!(
                    "Failed to remove invalid permission-denied retention index: {error}"
                ))
            })?;
        }

        on_progress(MaintenanceProgress::IndexStarted {
            name: PERMISSION_DENIED_RETENTION_INDEX,
        });
        let started = std::time::Instant::now();

        sqlx::query(
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS \
                 idx_audit_logs_permission_denied_retention \
             ON audit_logs (audit_date ASC, id ASC) \
             WHERE operation_type = 'PERMISSION_DENIED'",
        )
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to build concurrent permission-denied retention index \
                 '{PERMISSION_DENIED_RETENTION_INDEX}': {error}"
            ))
        })?;

        on_progress(MaintenanceProgress::IndexFinished {
            name: PERMISSION_DENIED_RETENTION_INDEX,
            elapsed: started.elapsed(),
        });

        // The reference indexes scan whole tables and run without a
        // statement timeout: a build that is cut off at a fixed limit would
        // be restarted from nothing on every retry and never finish on a
        // large history. They do not block writes (see `ReferenceIndex`), and
        // the lock timeout above still bounds waiting for a lock.
        sqlx::query("SET statement_timeout = 0")
            .execute(&mut *connection)
            .await
            .map_err(|error| {
                ServiceError::Database(format!(
                    "Failed to lift the statement timeout for reference index builds: {error}"
                ))
            })?;
        build_reference_indexes(&mut connection, &on_progress).await?;

        Ok(())
    }
    .await;

    // Do not leak maintenance-specific session settings back into the pool.
    let reset_lock_result = sqlx::query("RESET lock_timeout")
        .execute(&mut *connection)
        .await;
    let reset_statement_result = sqlx::query("RESET statement_timeout")
        .execute(&mut *connection)
        .await;

    maintenance_result?;
    reset_lock_result.map_err(|error| {
        ServiceError::Database(format!(
            "Built permission-denied retention index but failed to reset lock timeout: {error}"
        ))
    })?;
    reset_statement_result.map_err(|error| {
        ServiceError::Database(format!(
            "Built permission-denied retention index but failed to reset statement timeout: {error}"
        ))
    })?;

    Ok(())
}

/// Run post-migration reconciliation and continuous-aggregate backfills.
///
/// Long trace-summary reconciliation and `CALL refresh_continuous_aggregate()`
/// cannot safely run inside SeaORM's migration transaction. This function runs
/// that work after the migration transaction has committed.
///
/// This is idempotent and safe to call on every startup. Trace reconciliation
/// uses durable pending state and an atomic shadow-table cutover; refreshing an
/// already-populated aggregate is a no-op for unchanged data.
///
/// Run this on a long-lived runtime (e.g. via `tokio::spawn`) so it never blocks
/// startup; it is decoupled from `establish_connection` for exactly that reason.
/// One continuous-aggregate refresh window: optional start (`None` = NULL,
/// i.e. unbounded) and end, both as SQL expressions.
type RefreshWindow = (Option<String>, String);

pub async fn run_post_migration_backfill(db: &DatabaseConnection) -> ServiceResult<()> {
    run_post_migration_backfill_streaming(db, |_| {}).await
}

/// [`run_post_migration_backfill`] with live progress events.
///
/// One [`MaintenanceProgress::AggregateWindow`] is emitted per refresh window,
/// so an interactive caller can show "window 7/31" instead of nothing at all
/// while a multi-minute backfill runs.
pub async fn run_post_migration_backfill_streaming<F>(
    db: &DatabaseConnection,
    on_progress: F,
) -> ServiceResult<()>
where
    F: Fn(MaintenanceProgress<'_>),
{
    reconcile_otel_trace_summaries(db).await?;

    // Report the backend that will run the refresh calls, so an interrupt can
    // cancel THIS one. The migrate pool is capped at a single connection, so the
    // PID read here is the PID every `refresh_continuous_aggregate()` below runs
    // on — and it differs from the connect-time PID whenever a disposable
    // maintenance connection (see `run_post_migration_indexes_streaming`) has
    // already been retired.
    if let Ok(Some(row)) = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT pg_backend_pid() AS pid".to_owned(),
        ))
        .await
    {
        if let Ok(pid) = row.try_get::<i32>("", "pid") {
            on_progress(MaintenanceProgress::PhaseBackend { pid });
        }
    }

    // Refresh windows per aggregate, executed in order. `None` as a bound
    // means unbounded (NULL). Window ends mirror each aggregate's
    // refresh-policy end_offset so the backfill never overlaps the region
    // the policy is responsible for.
    //
    // `proxy_logs_stats_1m` is chunked into 1-day windows, NEWEST FIRST,
    // instead of one unbounded call, for two reasons on large installs
    // (100M+ retained rows):
    //   * Each `CALL refresh_continuous_aggregate()` is one transaction; a
    //     single 30-day refresh over that volume is a multi-minute
    //     transaction with heavy WAL churn. Day-sized chunks keep each
    //     transaction small.
    //   * Once the every-minute refresh policy runs, the aggregate's
    //     watermark advances and real-time aggregation stops covering
    //     older, still-unmaterialized regions — queries would under-report
    //     history until the backfill reaches it. Newest-first chunking
    //     makes the dashboard's common windows (1h/6h/24h) correct after
    //     the first chunk, then works backwards.
    // The final unbounded window sweeps anything older than the loop
    // covers (e.g. rows the 30-day retention hasn't dropped yet); it is a
    // no-op when empty.
    let proxy_stats_windows: Vec<RefreshWindow> = (1..=30)
        .map(|d| {
            let start = Some(format!("NOW() - INTERVAL '{d} days'"));
            let end = if d == 1 {
                "NOW() - INTERVAL '1 minute'".to_string()
            } else {
                format!("NOW() - INTERVAL '{} days'", d - 1)
            };
            (start, end)
        })
        .chain(std::iter::once((
            None,
            "NOW() - INTERVAL '30 days'".to_string(),
        )))
        .collect();

    let aggregates: [(&str, Vec<RefreshWindow>); 2] = [
        (
            "events_hourly",
            vec![(None, "NOW() - INTERVAL '1 hour'".to_string())],
        ),
        ("proxy_logs_stats_1m", proxy_stats_windows),
    ];

    for (view_name, windows) in aggregates {
        // Check the aggregate exists before attempting backfill (it may not
        // if the creating migration hasn't run on this install yet).
        let check_sql = format!(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM timescaledb_information.continuous_aggregates
                WHERE view_name = '{view_name}'
            ) as exists
            "#
        );

        let row = db
            .query_one(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                check_sql,
            ))
            .await
            .map_err(|e| {
                ServiceError::Database(format!("Failed to check for {view_name} aggregate: {e}"))
            })?;

        let exists = row
            .map(|r| r.try_get("", "exists").unwrap_or(false))
            .unwrap_or(false);
        if !exists {
            continue;
        }

        debug!("Backfilling {view_name} continuous aggregate");
        let total_windows = windows.len();
        on_progress(MaintenanceProgress::AggregateStarted {
            view: view_name,
            windows: total_windows,
        });
        let aggregate_started = std::time::Instant::now();
        let mut failed = 0usize;
        for (position, (window_start, window_end)) in windows.iter().enumerate() {
            let start_sql = window_start.as_deref().unwrap_or("NULL");
            let backfill_sql = format!(
                "CALL refresh_continuous_aggregate('{view_name}', {start_sql}, {window_end})"
            );
            let window_started = std::time::Instant::now();
            let window_error = if let Err(e) = db
                .execute(Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    backfill_sql,
                ))
                .await
            {
                // Log but keep going — later windows are independent, and
                // the refresh policy will eventually catch up regardless.
                failed += 1;
                tracing::warn!(
                    "Failed to backfill {view_name} window [{start_sql}, {window_end}] \
                     (refresh policy will catch up): {e}"
                );
                Some(e.to_string())
            } else {
                None
            };
            on_progress(MaintenanceProgress::AggregateWindow {
                view: view_name,
                index: position + 1,
                total: total_windows,
                elapsed: window_started.elapsed(),
                error: window_error,
            });
        }
        on_progress(MaintenanceProgress::AggregateFinished {
            view: view_name,
            windows: total_windows,
            failed,
            elapsed: aggregate_started.elapsed(),
        });
        if failed == 0 {
            debug!("{view_name} continuous aggregate backfill complete");
        } else {
            tracing::warn!(
                "{view_name} continuous aggregate backfill finished with {failed} of {} windows failed",
                windows.len()
            );
        }
    }

    Ok(())
}

pub async fn reconcile_otel_trace_summaries(db: &DatabaseConnection) -> ServiceResult<()> {
    let pending = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT CASE WHEN to_regclass('otel_trace_summary_rebuild_state') IS NULL \
                 THEN FALSE ELSE EXISTS (SELECT 1 FROM otel_trace_summary_rebuild_state \
                 WHERE NOT completed) END AS pending"
                .to_string(),
        ))
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to inspect trace-summary rebuild state: {error}"
            ))
        })?
        .and_then(|row| row.try_get::<bool>("", "pending").ok())
        .unwrap_or(false);
    if !pending {
        return Ok(());
    }

    // A named shadow table is shared by every process using this database.
    // Serialize the complete rebuild on one disposable session, then re-check
    // state after acquiring the lock in case another replica just finished it.
    let pool = db.get_postgres_connection_pool();
    let mut connection = pool.acquire().await.map_err(|error| {
        ServiceError::Database(format!(
            "Failed to acquire trace-summary reconciliation connection: {error}"
        ))
    })?;
    connection.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock(hashtext('temps:otel_trace_summary_rebuild')::BIGINT)")
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to lock trace-summary reconciliation: {error}"
            ))
        })?;
    let pending = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM otel_trace_summary_rebuild_state WHERE NOT completed)",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| {
        ServiceError::Database(format!(
            "Failed to re-check trace-summary rebuild state: {error}"
        ))
    })?;
    if !pending {
        return Ok(());
    }

    // Take the watermark while inserts are briefly stopped. This makes the ID
    // boundary commit-safe: every transaction that allocated/inserts an older
    // ID has finished before MAX(id) is observed.
    let mut watermark_tx = connection.begin().await.map_err(|error| {
        ServiceError::Database(format!("Failed to begin trace-summary watermark: {error}"))
    })?;
    sqlx::query("LOCK TABLE otel_spans IN SHARE MODE")
        .execute(&mut *watermark_tx)
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to lock spans for summary watermark: {error}"
            ))
        })?;
    sqlx::query(
        "UPDATE otel_trace_summary_rebuild_state \
             SET watermark = (SELECT COALESCE(MAX(id), 0) FROM otel_spans), updated_at = now() \
             WHERE NOT completed",
    )
    .execute(&mut *watermark_tx)
    .await
    .map_err(|error| {
        ServiceError::Database(format!("Failed to record trace-summary watermark: {error}"))
    })?;
    watermark_tx.commit().await.map_err(|error| {
        ServiceError::Database(format!("Failed to commit trace-summary watermark: {error}"))
    })?;

    sqlx::query("DROP TABLE IF EXISTS otel_trace_summaries_rebuild CASCADE")
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to remove stale trace-summary shadow table: {error}"
            ))
        })?;
    sqlx::query(
        "CREATE TABLE otel_trace_summaries_rebuild \
             (LIKE otel_trace_summaries INCLUDING ALL)",
    )
    .execute(&mut *connection)
    .await
    .map_err(|error| {
        ServiceError::Database(format!(
            "Failed to create trace-summary shadow table: {error}"
        ))
    })?;
    // Build indexes on the offline shadow table. Creating them on the live
    // summary table inside the schema migration would retain a write-blocking
    // table lock for the full index scan and, because span + summary writes
    // are atomic, would stall ingestion on large upgrades.
    for statement in [
        "CREATE INDEX otel_trace_summaries_rebuild_project_last_span_start \
         ON otel_trace_summaries_rebuild (project_id, last_span_start_time DESC)",
        "CREATE INDEX otel_trace_summaries_rebuild_last_span_start \
         ON otel_trace_summaries_rebuild (last_span_start_time)",
    ] {
        sqlx::query(statement)
            .execute(&mut *connection)
            .await
            .map_err(|error| {
                ServiceError::Database(format!(
                    "Failed to index trace-summary shadow table while executing '{statement}': {error}"
                ))
            })?;
    }
    let initial_rebuild_sql = temps_migrations::trace_summary_rebuild_initial_sql();
    sqlx::query(&initial_rebuild_sql)
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to populate trace-summary shadow table: {error}"
            ))
        })?;

    let mut cutover = connection.begin().await.map_err(|error| {
        ServiceError::Database(format!("Failed to begin trace-summary cutover: {error}"))
    })?;
    sqlx::query("LOCK TABLE otel_spans IN SHARE MODE")
        .execute(&mut *cutover)
        .await
        .map_err(|error| {
            ServiceError::Database(format!("Failed to lock spans for summary cutover: {error}"))
        })?;
    let delta_rebuild_sql = temps_migrations::trace_summary_rebuild_delta_sql();
    sqlx::query(&delta_rebuild_sql)
        .execute(&mut *cutover)
        .await
        .map_err(|error| {
            ServiceError::Database(format!(
                "Failed to apply trace-summary rebuild delta: {error}"
            ))
        })?;
    for statement in [
        "LOCK TABLE otel_trace_summaries IN ACCESS EXCLUSIVE MODE",
        "ALTER TABLE otel_trace_summaries RENAME TO otel_trace_summaries_old",
        "ALTER TABLE otel_trace_summaries_rebuild RENAME TO otel_trace_summaries",
        "DROP TABLE otel_trace_summaries_old CASCADE",
        "UPDATE otel_trace_summary_rebuild_state \
         SET completed = TRUE, updated_at = now() WHERE NOT completed",
    ] {
        sqlx::query(statement)
            .execute(&mut *cutover)
            .await
            .map_err(|error| {
                ServiceError::Database(format!(
                    "Failed to swap rebuilt trace summaries while executing '{statement}': {error}"
                ))
            })?;
    }
    cutover.commit().await.map_err(|error| {
        ServiceError::Database(format!("Failed to commit rebuilt trace summaries: {error}"))
    })?;

    debug!("OTel trace-summary reconciliation complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_table_reference_indexes_build_concurrently() {
        for index in REFERENCE_INDEXES.iter().filter(|i| !i.hypertable) {
            let sql = index.create_sql();
            assert!(
                sql.starts_with("CREATE INDEX CONCURRENTLY IF NOT EXISTS "),
                "{sql}"
            );
            assert!(!sql.contains("transaction_per_chunk"), "{sql}");
            assert!(
                index
                    .drop_invalid_sql()
                    .starts_with("DROP INDEX CONCURRENTLY IF EXISTS "),
                "dropping a leftover must not take a write-blocking lock"
            );
        }
    }

    /// TimescaleDB rejects `CREATE INDEX CONCURRENTLY` on a hypertable.
    #[test]
    fn hypertable_reference_indexes_build_one_chunk_per_transaction() {
        // TimescaleDB rejects CREATE INDEX CONCURRENTLY on these; this list is
        // the hypertables among the indexed tables (see the migrations).
        let mut hypertables: Vec<_> = REFERENCE_INDEXES
            .iter()
            .filter(|i| i.hypertable)
            .map(|i| i.table)
            .collect();
        hypertables.sort_unstable();
        assert_eq!(hypertables, ["error_events", "proxy_logs"]);

        for index in REFERENCE_INDEXES.iter().filter(|i| i.hypertable) {
            let sql = index.create_sql();
            assert!(!sql.contains("CONCURRENTLY"), "{sql}");
            assert!(
                sql.contains("WITH (timescaledb.transaction_per_chunk)"),
                "{sql}"
            );
            // `WITH` must come before `WHERE` in the CREATE INDEX grammar.
            assert!(
                sql.find("WITH (").expect("with") < sql.find("WHERE").expect("where"),
                "{sql}"
            );
            assert_eq!(
                index.drop_invalid_sql(),
                format!("DROP INDEX IF EXISTS {}", index.name)
            );
        }
    }

    #[test]
    fn reference_indexes_are_partial_except_last_seen_and_names_are_unique() {
        for index in REFERENCE_INDEXES {
            let sql = index.create_sql();
            let partial = sql.contains(&format!("WHERE {} IS NOT NULL", index.column));
            assert_eq!(partial, index.column != "last_seen", "{sql}");
            assert!(sql.contains("IF NOT EXISTS"), "{sql}");
        }
        let mut names: Vec<_> = REFERENCE_INDEXES.iter().map(|i| i.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), REFERENCE_INDEXES.len());
    }
    use std::sync::atomic::{AtomicU32, Ordering};
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

    #[test]
    fn startup_retry_backs_off_exponentially_up_to_a_ceiling() {
        let delays: Vec<u64> = (1..=8)
            .map(|attempt| startup_retry_delay(attempt).as_secs())
            .collect();
        assert_eq!(delays, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(startup_retry_delay(u32::MAX), STARTUP_RETRY_MAX_DELAY);
    }

    #[tokio::test(start_paused = true)]
    async fn startup_wait_retries_transient_failures_until_the_database_answers() {
        let attempts = AtomicU32::new(0);
        let started = tokio::time::Instant::now();
        let result = retry_database_startup("db:5432", Duration::from_secs(60), || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if attempt < 3 {
                    Err(StartupAttemptFailure::Transient(
                        "Connection refused (os error 61)".to_string(),
                    ))
                } else {
                    Ok(attempt)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 3);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        // Slept 1 s, then 2 s.
        assert_eq!(started.elapsed().as_secs(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn startup_wait_gives_up_after_the_limit_with_a_clear_error() {
        let attempts = AtomicU32::new(0);
        let started = tokio::time::Instant::now();
        let error = retry_database_startup("db:5432", Duration::from_secs(10), || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(StartupAttemptFailure::Transient(
                    "No route to host (os error 113)".to_string(),
                ))
            }
        })
        .await
        .unwrap_err()
        .to_string();
        // Attempts at 0, 1, 3 and 7 s; the next one (8 s later) would start
        // past the limit, so it waits out the rest and gives up at 10 s.
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
        assert_eq!(started.elapsed().as_secs(), 10);
        for expected in [
            "db:5432",
            "after waiting 10s",
            "4 attempt(s)",
            "No route to host",
            "TEMPS_DB_STARTUP_WAIT",
        ] {
            assert!(error.contains(expected), "{expected:?} missing: {error}");
        }
    }

    /// A probe that hangs (a black-holed host) must not carry startup past
    /// the limit: the attempt is cut off when the wait runs out.
    #[tokio::test(start_paused = true)]
    async fn a_slow_attempt_cannot_outlast_the_startup_wait() {
        let attempts = AtomicU32::new(0);
        let started = tokio::time::Instant::now();
        let error = retry_database_startup("db:5432", Duration::from_secs(5), || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<(), StartupAttemptFailure>(())
            }
        })
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(started.elapsed().as_secs(), 5);
        assert!(error.contains("did not finish within the 5s"), "{error}");
        assert!(error.contains("after waiting 5s"), "{error}");
    }

    /// With no wait configured the single attempt keeps its own timeouts and
    /// is not cut short.
    #[tokio::test(start_paused = true)]
    async fn a_zero_wait_does_not_cut_the_single_attempt_short() {
        let result = retry_database_startup("db:5432", Duration::ZERO, || async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            Ok::<u8, StartupAttemptFailure>(7)
        })
        .await;
        assert_eq!(result.unwrap(), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn startup_wait_stops_at_once_when_waiting_cannot_help() {
        let attempts = AtomicU32::new(0);
        let error = retry_database_startup("db:5432", Duration::from_secs(60), || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(StartupAttemptFailure::Permanent(
                    "password authentication failed for user \"temps\"".to_string(),
                ))
            }
        })
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(error.contains("password authentication failed"), "{error}");
        assert!(error.contains("Waiting would not help"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_startup_wait_makes_exactly_one_attempt() {
        let attempts = AtomicU32::new(0);
        let result = retry_database_startup("db:5432", Duration::ZERO, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(StartupAttemptFailure::Transient(
                    "Connection refused".to_string(),
                ))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn connection_errors_are_classified_by_whether_waiting_can_help() {
        let refused = sea_orm::DbErr::Conn(sea_orm::RuntimeErr::SqlxError(sqlx::Error::Io(
            std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        )));
        assert!(matches!(
            classify_connect_error(&refused),
            StartupAttemptFailure::Transient(_)
        ));
        let pool_timeout =
            sea_orm::DbErr::Conn(sea_orm::RuntimeErr::SqlxError(sqlx::Error::PoolTimedOut));
        assert!(matches!(
            classify_connect_error(&pool_timeout),
            StartupAttemptFailure::Transient(_)
        ));
        let bad_url = sea_orm::DbErr::Conn(sea_orm::RuntimeErr::SqlxError(
            sqlx::Error::Configuration("invalid port".into()),
        ));
        assert!(matches!(
            classify_connect_error(&bad_url),
            StartupAttemptFailure::Permanent(_)
        ));
    }

    /// Real sockets, real clock: nothing listens on the port, so every
    /// attempt is refused and the wait ends with the final error.
    #[tokio::test]
    async fn wait_for_database_reports_an_unreachable_database_after_the_limit() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("postgres://temps:temps@127.0.0.1:{port}/temps");

        let started = std::time::Instant::now();
        let error = wait_for_database(&url, Duration::from_secs(2))
            .await
            .unwrap_err()
            .to_string();
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(2),
            "gave up early: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(10), "overshot: {elapsed:?}");
        assert!(error.contains(&format!("127.0.0.1:{port}")), "{error}");
        assert!(error.contains("still unreachable"), "{error}");
    }

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

    #[test]
    fn connect_options_logs_statements_at_trace_and_slow_ones_at_warn() {
        let opt = connect_options("postgres://localhost/db");
        assert!(opt.get_sqlx_logging());
        assert_eq!(opt.get_sqlx_logging_level(), log::LevelFilter::Trace);
        assert_eq!(
            opt.get_sqlx_slow_statements_logging_settings(),
            (log::LevelFilter::Warn, SLOW_STATEMENT_THRESHOLD)
        );
    }

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs `queries` over a `connect_options` connection with a subscriber
    /// using `filter`, returning everything it logged.
    async fn capture_sql_logs(
        database_url: &str,
        filter: &str,
        queries: &[&str],
    ) -> anyhow::Result<String> {
        let buffer = LogBuffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut opt = connect_options(database_url);
        opt.max_connections(1).min_connections(1);
        let db = Database::connect(opt).await?;
        for query in queries {
            db.execute(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                (*query).to_owned(),
            ))
            .await?;
        }
        db.close().await?;

        let logs = String::from_utf8(buffer.0.lock().unwrap().clone())?;
        Ok(logs)
    }

    #[tokio::test]
    async fn statements_stay_out_of_info_logs_but_slow_statements_warn() -> anyhow::Result<()> {
        let container = match GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(testcontainers::core::ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error)
                if crate::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping Docker-dependent SQL logging test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let host = container.get_host().await?.to_string();
        let port = container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres@{host}:{port}/postgres");

        // An operator running with RUST_LOG=info sees no per-query SQL, but
        // still sees the query that crossed the slow threshold.
        let info_logs = capture_sql_logs(
            &database_url,
            "info",
            &[
                "SELECT 'fast_marker'",
                "SELECT pg_sleep(1.2), 'slow_marker'",
            ],
        )
        .await?;
        assert!(
            !info_logs.contains("fast_marker"),
            "fast statement leaked into INFO logs:\n{info_logs}"
        );
        assert!(
            info_logs.contains("WARN") && info_logs.contains("slow_marker"),
            "slow statement was not reported at WARN:\n{info_logs}"
        );

        // Per-query SQL is still available on explicit opt-in.
        let trace_logs = capture_sql_logs(
            &database_url,
            "sqlx::query=trace",
            &["SELECT 'fast_marker'"],
        )
        .await?;
        assert!(
            trace_logs.contains("TRACE") && trace_logs.contains("fast_marker"),
            "statement missing from sqlx::query=trace logs:\n{trace_logs}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn cancellation_is_confirmed_and_scoped_to_the_captured_backend() -> anyhow::Result<()> {
        let container = match GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(testcontainers::core::ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error)
                if crate::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping Docker-dependent migration cancellation test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let host = container.get_host().await?.to_string();
        let port = container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres@{host}:{port}/postgres");

        let (target_db, target_pid) = connect_for_migrate(&database_url).await?;
        let mut other_options = ConnectOptions::new(&database_url);
        other_options.max_connections(1).min_connections(1);
        let other_db = Database::connect(other_options).await?;
        other_db
            .execute(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SET application_name = 'temps_migrate'".to_owned(),
            ))
            .await?;
        let other_pid = other_db
            .query_one(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT pg_backend_pid() AS pid".to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("second migrator returned no backend PID"))?
            .try_get::<i32>("", "pid")?;

        let target_task = {
            let db = target_db.clone();
            tokio::spawn(async move {
                db.query_one(Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT pg_sleep(30)".to_owned(),
                ))
                .await
            })
        };
        let other_task = tokio::spawn(async move {
            other_db
                .query_one(Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT pg_sleep(1)".to_owned(),
                ))
                .await
        });

        let observer = Database::connect(&database_url).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let active = observer
                .query_one(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT COUNT(*)::bigint AS active FROM pg_stat_activity \
                     WHERE pid IN ($1, $2) AND state = 'active'",
                    [target_pid.into(), other_pid.into()],
                ))
                .await?
                .ok_or_else(|| anyhow::anyhow!("activity query returned no row"))?
                .try_get::<i64>("", "active")?;
            if active == 2 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("migration backends did not become active before cancellation test");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        cancel_migration_backend(&database_url, target_pid).await?;
        let target_result = tokio::time::timeout(Duration::from_secs(5), target_task).await??;
        assert!(
            target_result.is_err(),
            "captured migration backend query must be cancelled"
        );
        let other_result = tokio::time::timeout(Duration::from_secs(5), other_task).await??;
        assert!(
            other_result.is_ok(),
            "a separate temps_migrate backend must not be cancelled"
        );

        Ok(())
    }

    /// Cancelling a backend that has already gone away must SUCCEED.
    ///
    /// `pg_cancel_backend()` returns false (with a "PID N is not a PostgreSQL
    /// backend process" warning) for a PID that no longer exists. Treating that
    /// as a failure told the operator "PostgreSQL did not confirm cancellation
    /// … do not rerun migrations" when in fact nothing was left running — the
    /// exact false alarm reported after interrupting post-migration maintenance,
    /// whose backend differs from the connect-time one.
    #[tokio::test]
    async fn cancelling_an_already_gone_backend_succeeds() -> anyhow::Result<()> {
        let container = match GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(testcontainers::core::ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error)
                if crate::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping Docker-dependent gone-backend cancellation test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let host = container.get_host().await?.to_string();
        let port = container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres@{host}:{port}/postgres");

        // Capture a real PID, then close its connection so the backend is gone.
        let (db, pid) = connect_for_migrate(&database_url).await?;
        db.get_postgres_connection_pool().close().await;
        drop(db);

        let observer = Database::connect(&database_url).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let present = observer
                .query_one(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1) AS present",
                    [pid.into()],
                ))
                .await?
                .ok_or_else(|| anyhow::anyhow!("activity query returned no row"))?
                .try_get::<bool>("", "present")?;
            if !present {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("closed migration backend never left pg_stat_activity");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        cancel_migration_backend(&database_url, pid)
            .await
            .map_err(|error| {
                anyhow::anyhow!("cancelling an already-gone backend must succeed: {error}")
            })?;

        Ok(())
    }
}
