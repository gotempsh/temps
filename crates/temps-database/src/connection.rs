// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database connection management

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use std::sync::Arc;
use std::time::Duration;
use temps_core::{ServiceError, ServiceResult};
use tokio::net::TcpStream;
use tokio::time::timeout;

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

/// Open the server's connection pool, sized by `TEMPS_DB_*`, without touching
/// the schema. `temps_schema::establish_connection` builds on this.
pub async fn connect_pooled(database_url: &str) -> ServiceResult<DbConnection> {
    // Parse the database URL to extract host and port
    let (host, port) = parse_database_url(database_url)
        .map_err(|e| ServiceError::Database(format!("Invalid database URL: {}", e)))?;

    // Check if the database is reachable before attempting to connect
    check_database_connectivity(&host, port)
        .await
        .map_err(ServiceError::Database)?;

    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: DB pool size is needed to open the database connection"
    )]
    let max_conn: u32 = std::env::var("TEMPS_DB_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: DB pool size is needed to open the database connection"
    )]
    let min_conn: u32 = std::env::var("TEMPS_DB_MIN_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: DB pool timeout is needed to open the database connection"
    )]
    let acquire_timeout_secs: u64 = std::env::var("TEMPS_DB_ACQUIRE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    #[allow(
        clippy::disallowed_methods,
        reason = "bootstrap: DB pool timeout is needed to open the database connection"
    )]
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
    match timeout(CONNECTION_TIMEOUT, Database::connect(opt)).await {
        Ok(Ok(db)) => Ok(db),
        Ok(Err(e)) => Err(ServiceError::Database(format!(
            "Failed to connect to database: {}",
            e
        ))),
        Err(_) => Err(ServiceError::Database(format!(
            "Database connection timed out after {} seconds",
            CONNECTION_TIMEOUT.as_secs()
        ))),
    }
}

/// Connect to the database WITHOUT running migrations.
///
/// Used by the standalone `temps migrate` command (which runs migrations
/// explicitly afterwards) and by any caller that must not trigger schema
/// changes. Performs the same connectivity check as [`connect_pooled`], with
/// Sea-ORM's default pool of 10 connections, which ignores `TEMPS_DB_*`. A
/// long-running server wants [`connect_pooled`].
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Keep a second tracing dispatcher registered for the life of the test
    /// process, so a scoped subscriber is never the only one.
    ///
    /// While exactly one dispatcher is registered, `tracing-core` computes the
    /// "interest" of a callsite that registers for the first time from the
    /// *calling thread's* default subscriber, and caches the answer for every
    /// thread. The tests in this crate run in parallel: when another test's
    /// first query registers sqlx's `sqlx::query` callsites on a thread with
    /// no subscriber, the cached answer is "never", and the capture below
    /// then records nothing at all, not even the slow-statement warning. With a
    /// second dispatcher registered, `tracing-core` combines every registered
    /// dispatcher instead, and each event is checked against the subscriber of
    /// the thread that emits it.
    fn keep_dispatchers_from_being_alone() {
        static SECOND_DISPATCHER: std::sync::OnceLock<tracing::Dispatch> =
            std::sync::OnceLock::new();
        SECOND_DISPATCHER
            .get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    }

    /// Runs `queries` over a `connect_options` connection with a subscriber
    /// using `filter`, returning everything it logged.
    async fn capture_sql_logs(
        database_url: &str,
        filter: &str,
        queries: &[&str],
    ) -> anyhow::Result<String> {
        keep_dispatchers_from_being_alone();
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
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
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
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
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
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
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
