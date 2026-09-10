// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

    let mut opt = ConnectOptions::new(database_url);
    // Exactly one backend, kept alive for the run, so the PID below is the
    // backend that executes every migration statement.
    opt.max_connections(1)
        .min_connections(1)
        .sqlx_logging(false);

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
    let db = Database::connect(ConnectOptions::new(database_url))
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

const PERMISSION_DENIED_RETENTION_INDEX: &str = "idx_audit_logs_permission_denied_retention";

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

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

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
