// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Post-migration maintenance: concurrent index builds, trace-summary
//! reconciliation and continuous-aggregate backfills.

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use sqlx::Acquire;
use std::time::Duration;
use temps_core::{ServiceError, ServiceResult};
use tracing::debug;

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

/// Attempts at building one index before its failure is reported.
const INDEX_BUILD_ATTEMPTS: u32 = 4;

/// Pause before the first retry of an index build; each later retry waits
/// twice as long as the one before.
const INDEX_BUILD_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Whether PostgreSQL reported a failure that a rebuild can get past.
///
/// A concurrent build waits for every transaction that could still see the
/// table without the new index, and PostgreSQL ends that wait with
/// `deadlock_detected` (40P01) when two sessions would otherwise wait on each
/// other forever, or `lock_not_available` (55P03) when the maintenance
/// connection's lock timeout runs out. Neither says anything about the index
/// or the data: the other session simply finished a moment later, so the same
/// statement succeeds on the next attempt. Anything else (a missing column, a
/// full disk, a cancelled statement) will fail again and is reported at once.
fn is_transient_index_build_sqlstate(code: &str) -> bool {
    matches!(code, "40P01" | "55P03")
}

fn is_transient_index_build_error(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|database_error| database_error.code())
        .is_some_and(|code| is_transient_index_build_sqlstate(&code))
}

/// How long to wait before the next attempt after `failed_attempts` failures
/// (1-based), or `None` when the failure must be reported: it is not
/// transient, or the attempts are used up.
fn index_build_retry_delay(failed_attempts: u32, transient: bool) -> Option<Duration> {
    if !transient || failed_attempts >= INDEX_BUILD_ATTEMPTS {
        return None;
    }
    Some(INDEX_BUILD_RETRY_DELAY * 2u32.pow(failed_attempts.saturating_sub(1)))
}

/// Why one attempt at building an index failed.
struct IndexAttemptError {
    /// What failed, naming the index, ready to appear in a report.
    message: String,
    transient: bool,
}

/// One attempt at building `name`: clear the invalid leftover of an earlier
/// failed build, then create it.
///
/// The cleanup is part of the attempt, not a one-off beforehand. A concurrent
/// build that fails midway leaves an invalid index behind, and `IF NOT
/// EXISTS` would treat that leftover as the finished index and succeed
/// without building anything.
async fn build_index_once(
    connection: &mut sqlx::PgConnection,
    name: &str,
    target: &str,
    create_sql: &str,
    drop_invalid_sql: &str,
) -> Result<(), IndexAttemptError> {
    let failed = |step: String, error: sqlx::Error| IndexAttemptError {
        message: format!("{name} {step}: {error}"),
        transient: is_transient_index_build_error(&error),
    };

    let is_valid = sqlx::query_scalar::<_, bool>(
        "SELECT indisvalid FROM pg_index WHERE indexrelid = to_regclass($1)",
    )
    .bind(name)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| failed("(inspecting it)".to_string(), error))?;
    if is_valid == Some(false) {
        sqlx::query(drop_invalid_sql)
            .execute(&mut *connection)
            .await
            .map_err(|error| failed("(removing an invalid earlier build)".to_string(), error))?;
    }

    sqlx::query(create_sql)
        .execute(&mut *connection)
        .await
        .map_err(|error| failed(format!("on {target}"), error))?;
    Ok(())
}

/// Build `name`, retrying the failures [`is_transient_index_build_sqlstate`]
/// recognises up to [`INDEX_BUILD_ATTEMPTS`] times in total.
///
/// The error is the text to report: the last attempt's failure, with the
/// attempt count when more than one was made.
async fn build_index_with_retry(
    connection: &mut sqlx::PgConnection,
    name: &str,
    target: &str,
    create_sql: &str,
    drop_invalid_sql: &str,
) -> Result<(), String> {
    let mut failed_attempts: u32 = 0;
    loop {
        let failure =
            match build_index_once(&mut *connection, name, target, create_sql, drop_invalid_sql)
                .await
            {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
        failed_attempts += 1;
        let Some(delay) = index_build_retry_delay(failed_attempts, failure.transient) else {
            return Err(if failed_attempts > 1 {
                format!("{} (after {failed_attempts} attempts)", failure.message)
            } else {
                failure.message
            });
        };
        tracing::warn!(
            index = name,
            attempt = failed_attempts,
            max_attempts = INDEX_BUILD_ATTEMPTS,
            retry_in_ms = delay.as_millis() as u64,
            "Concurrent build of index {name} hit a transient failure and will be retried: {}",
            failure.message
        );
        tokio::time::sleep(delay).await;
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

        on_progress(MaintenanceProgress::IndexStarted { name: index.name });
        let started = std::time::Instant::now();
        let target = format!("{}({})", index.table, index.column);
        match build_index_with_retry(
            &mut *connection,
            index.name,
            &target,
            &index.create_sql(),
            &index.drop_invalid_sql(),
        )
        .await
        {
            Ok(()) => on_progress(MaintenanceProgress::IndexFinished {
                name: index.name,
                elapsed: started.elapsed(),
            }),
            Err(message) => failures.push(message),
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

/// Whether `table.column` exists and lacks a valid, non-partial index that
/// leads with it.
///
/// An invalid index (a failed concurrent build) does not count, so the
/// leftover of an earlier failed attempt is rebuilt rather than trusted. A
/// partial index does not count either: the cascade and the purger look rows
/// up by the column alone, which a predicate-restricted index can't serve.
const NEEDS_FOREIGN_KEY_INDEX_SQL: &str = "\
    SELECT a.attnum IS NOT NULL AND NOT EXISTS ( \
        SELECT 1 FROM pg_index i \
        WHERE i.indrelid = to_regclass($1) AND i.indkey[0] = a.attnum \
          AND i.indisvalid AND i.indpred IS NULL \
    ) \
    FROM (SELECT 1) AS one \
    LEFT JOIN pg_attribute a \
      ON a.attrelid = to_regclass($1) AND a.attname = $2 AND NOT a.attisdropped";

/// Session-level advisory lock serializing [`build_project_foreign_key_indexes`]
/// across processes.
const PROJECT_FK_INDEX_LOCK_SQL: &str =
    "SELECT pg_advisory_lock(hashtext('temps:project_fk_indexes')::BIGINT)";

/// Index every foreign key column under `projects` that lacks one
/// ([`temps_migrations::PROJECT_FK_INDEXES`]), so a project delete cascades
/// with an index lookup per parent row instead of a table scan.
///
/// All targets are plain tables, so each builds with `CONCURRENTLY` and
/// writes continue during the scan. A column that already has an index
/// leading with it is skipped, whatever that index is called. Like
/// [`build_reference_indexes`], every index is attempted and the failures are
/// reported together.
async fn build_project_foreign_key_indexes<F>(
    connection: &mut sqlx::PgConnection,
    on_progress: &F,
) -> ServiceResult<()>
where
    F: Fn(MaintenanceProgress<'_>),
{
    // Serialize this set across every process on the database (several
    // control planes, or `temps migrate` next to `temps serve`). Otherwise one
    // process can find a column uncovered while another is still building
    // its index, and the drop below would remove that index once it is
    // done. The coverage check runs after the lock is held, so whoever waits
    // finds the other's indexes and skips them. The wait is unbounded, like
    // the trace-summary rebuild's: the holder is finite work, and the lock is
    // released when its disposable connection closes. The session's 5 s lock
    // timeout is restored for the builds.
    let locked = async {
        sqlx::query("SET lock_timeout = 0")
            .execute(&mut *connection)
            .await?;
        sqlx::query(PROJECT_FK_INDEX_LOCK_SQL)
            .execute(&mut *connection)
            .await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    let restored = sqlx::query("SET lock_timeout = '5s'")
        .execute(&mut *connection)
        .await;
    locked.map_err(|error| {
        ServiceError::Database(format!(
            "Failed to take the project foreign key index lock: {error}"
        ))
    })?;
    restored.map_err(|error| {
        ServiceError::Database(format!(
            "Failed to restore the lock timeout for project foreign key index builds: {error}"
        ))
    })?;

    let mut failures: Vec<String> = Vec::new();
    for (table, column) in temps_migrations::PROJECT_FK_INDEXES {
        let name = temps_migrations::project_fk_index_name(table, column);
        let needed = sqlx::query_scalar::<_, bool>(NEEDS_FOREIGN_KEY_INDEX_SQL)
            .bind(table)
            .bind(column)
            .fetch_one(&mut *connection)
            .await;
        match needed {
            Ok(true) => {}
            // Already indexed, or a table or column this installation lacks.
            Ok(false) => continue,
            Err(error) => {
                failures.push(format!("{name} (checking {table}.{column}): {error}"));
                continue;
            }
        }

        // The column is not covered, so an index already holding this name
        // (partial, or on another expression) is not a usable one, and
        // `IF NOT EXISTS` below would accept it and build nothing. The `_fk`
        // name belongs to this build, so replace whatever holds it.
        if let Err(error) = sqlx::query(&format!("DROP INDEX CONCURRENTLY IF EXISTS {name}"))
            .execute(&mut *connection)
            .await
        {
            failures.push(format!(
                "{name} (removing an unusable index of that name on {table}.{column}): {error}"
            ));
            continue;
        }

        on_progress(MaintenanceProgress::IndexStarted { name: &name });
        let started = std::time::Instant::now();
        match build_index_with_retry(
            &mut *connection,
            &name,
            &format!("{table}({column})"),
            &format!("CREATE INDEX CONCURRENTLY IF NOT EXISTS {name} ON {table} ({column})"),
            &format!("DROP INDEX CONCURRENTLY IF EXISTS {name}"),
        )
        .await
        {
            Ok(()) => on_progress(MaintenanceProgress::IndexFinished {
                name: &name,
                elapsed: started.elapsed(),
            }),
            Err(message) => failures.push(message),
        }
    }
    // Closing the disposable connection would release it too; release it now
    // so a waiting process doesn't also wait for the rest of maintenance.
    if let Err(error) =
        sqlx::query("SELECT pg_advisory_unlock(hashtext('temps:project_fk_indexes')::BIGINT)")
            .execute(&mut *connection)
            .await
    {
        tracing::warn!(
            "Failed to release the project foreign key index lock early; \
             it is released when the maintenance connection closes: {error}"
        );
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ServiceError::Database(format!(
            "Failed to build {} post-migration foreign key index(es): {}",
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
        on_progress(MaintenanceProgress::IndexStarted {
            name: PERMISSION_DENIED_RETENTION_INDEX,
        });
        let started = std::time::Instant::now();

        build_index_with_retry(
            &mut connection,
            PERMISSION_DENIED_RETENTION_INDEX,
            "audit_logs (audit_date, id)",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS \
                 idx_audit_logs_permission_denied_retention \
             ON audit_logs (audit_date ASC, id ASC) \
             WHERE operation_type = 'PERMISSION_DENIED'",
            "DROP INDEX CONCURRENTLY IF EXISTS idx_audit_logs_permission_denied_retention",
        )
        .await
        .map_err(|reason| {
            ServiceError::Database(format!(
                "Failed to build concurrent permission-denied retention index \
                 '{PERMISSION_DENIED_RETENTION_INDEX}': {reason}"
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
        // Both sets are attempted before either failure is returned, so one
        // stuck index does not hold back the other set until the next retry.
        let reference = build_reference_indexes(&mut connection, &on_progress).await;
        let foreign_keys = build_project_foreign_key_indexes(&mut connection, &on_progress).await;
        reference?;
        foreign_keys?;

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

    #[test]
    fn only_deadlocks_and_lock_timeouts_are_transient_index_build_failures() {
        assert!(
            is_transient_index_build_sqlstate("40P01"),
            "deadlock_detected"
        );
        assert!(
            is_transient_index_build_sqlstate("55P03"),
            "lock_not_available"
        );

        for permanent in [
            "57014", // query_canceled: an operator or statement timeout stopped it
            "42703", // undefined_column
            "42P07", // duplicate_table
            "53100", // disk_full
            "23505", // unique_violation
            "",
        ] {
            assert!(
                !is_transient_index_build_sqlstate(permanent),
                "SQLSTATE '{permanent}' will fail again and must not be retried"
            );
        }
    }

    #[test]
    fn errors_without_a_database_code_are_not_transient() {
        assert!(!is_transient_index_build_error(&sqlx::Error::PoolTimedOut));
        assert!(!is_transient_index_build_error(&sqlx::Error::RowNotFound));
    }

    #[test]
    fn index_build_retries_back_off_and_stop_after_the_attempt_budget() {
        assert_eq!(
            index_build_retry_delay(1, true),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            index_build_retry_delay(2, true),
            Some(Duration::from_millis(1000))
        );
        assert_eq!(
            index_build_retry_delay(3, true),
            Some(Duration::from_millis(2000))
        );
        assert_eq!(
            index_build_retry_delay(INDEX_BUILD_ATTEMPTS, true),
            None,
            "the last attempt's failure is reported, not retried"
        );
        assert_eq!(index_build_retry_delay(u32::MAX, true), None);
    }

    #[test]
    fn a_permanent_index_build_failure_is_never_retried() {
        for failed_attempts in 1..=INDEX_BUILD_ATTEMPTS {
            assert_eq!(index_build_retry_delay(failed_attempts, false), None);
        }
    }
}
