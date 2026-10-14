// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retention for analytics data and the identifiers attached to it.
//!
//! `events` and `proxy_logs` already expire through TimescaleDB retention
//! policies, but the rows they point at did not: `ip_geolocations` holds the
//! full client IP address and was never deleted, `visitor` rows were never
//! deleted, and `error_events` had no policy at all. This service enforces
//! the windows in [`AnalyticsRetentionSettings`]:
//!
//! - **`events` / `error_events`**: the TimescaleDB retention policy is kept
//!   in step with the setting (the same mechanism the observability tables
//!   use, so whole chunks are dropped rather than rows deleted).
//! - **`visitor`**: rows not seen for N days are deleted in bounded batches.
//!   Every foreign key to `visitor` is `SET NULL` or `CASCADE`.
//! - **`ip_geolocations`**: a row is deleted only when *nothing* references
//!   it, so the stored address lives exactly as long as the data that needs
//!   its location. An address referenced by an audit log is kept with it.
//!
//! The job is control-plane work: every pass is bounded (batch size and
//! batches per run), paced, and resumes where the previous run stopped, so a
//! large backlog is worked off over several runs instead of in one long
//! statement holding locks.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, FromQueryResult, Statement,
    TransactionTrait,
};
use serde::Serialize;
use temps_core::{AnalyticsRetentionSettings, AuditLogger, AuditOperation};
use thiserror::Error;
use tracing::{debug, info, warn};

/// How often the job re-reads the settings and enforces them.
pub const RETENTION_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Delay before the first pass, so startup is not competing with a sweep.
pub const RETENTION_STARTUP_DELAY: Duration = Duration::from_secs(5 * 60);
/// Rows removed (or examined) by one statement.
const BATCH_SIZE: i64 = 500;
/// Statements per pass per table. With [`BATCH_SIZE`] this bounds one pass at
/// 100k rows; an operator with a larger backlog catches up over several runs.
const MAX_BATCHES_PER_RUN: usize = 200;
/// Pause between batches, so the sweep yields to ingest.
const BATCH_PAUSE: Duration = Duration::from_millis(200);
/// Ceiling for `timescaledb.max_tuples_decompressed_per_dml_transaction` in a
/// visitor batch. The server default (100 000) is too low for the rows a
/// batch of visitors can own; "unlimited" (0) would let one batch decompress
/// whatever the oldest chunks hold. At most a few hundred MB of tuples.
const DECOMPRESSION_LIMIT: i64 = 500_000;
/// Visitors remembered as exceeding [`DECOMPRESSION_LIMIT`] on their own. The
/// list is bound into every scan, so it is capped; reaching it ends the pass
/// with a message naming the setting to raise.
const MAX_OVERSIZED_VISITORS: usize = 1000;
/// Passes (hours) between retries of the visitors that were skipped.
const OVERSIZED_RETRY_EVERY_PASSES: u32 = 24;

/// Tables whose retention is a TimescaleDB policy managed here.
const EVENTS_TABLE: &str = "events";
const ERROR_EVENTS_TABLE: &str = "error_events";

/// Every column that stores an `ip_geolocations.id`, as `(table, column)`.
///
/// Only some of these carry a foreign key (`events`, `error_events`,
/// `proxy_logs`); the rest are plain integers, so the database cannot be
/// relied on to refuse deleting a row that is still in use -- the cleanup
/// itself has to look. A column that does not exist in this installation is
/// skipped (see [`existing_reference_columns`]).
const IP_REFERENCES: &[(&str, &str)] = &[
    ("events", "ip_geolocation_id"),
    ("proxy_logs", "ip_geolocation_id"),
    ("error_events", "ip_geolocation_id"),
    ("visitor", "ip_address_id"),
    ("performance_metrics", "ip_address_id"),
    ("audit_logs", "ip_address_id"),
    ("request_logs", "ip_address_id"),
];

#[derive(Debug, Error)]
pub enum AnalyticsRetentionError {
    #[error("Failed to read analytics retention settings: {reason}")]
    Settings { reason: String },

    #[error("Failed to {operation} while enforcing analytics retention: {source}")]
    Database {
        operation: String,
        #[source]
        source: DbErr,
    },
}

impl AnalyticsRetentionError {
    fn database(operation: impl Into<String>, source: DbErr) -> Self {
        Self::Database {
            operation: operation.into(),
            source,
        }
    }
}

/// What a TimescaleDB retention policy needs to become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    /// The installed policy already matches.
    Keep,
    /// Install or replace the policy with this window.
    Set(u32),
    /// Remove the policy: the window is now "keep".
    Remove,
}

/// Decide what to do given the installed window and the configured one.
pub fn policy_action(installed_days: Option<u32>, desired_days: Option<u32>) -> PolicyAction {
    match (installed_days, desired_days) {
        (installed, Some(desired)) if installed == Some(desired) => PolicyAction::Keep,
        (_, Some(desired)) => PolicyAction::Set(desired),
        (Some(_), None) => PolicyAction::Remove,
        (None, None) => PolicyAction::Keep,
    }
}

/// Outcome of one pass, for logging and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub events_policy: Option<PolicyAction>,
    pub error_events_policy: Option<PolicyAction>,
    pub visitors_deleted: u64,
    pub ip_addresses_deleted: u64,
}

/// Position of the unreferenced-IP sweep: `(updated_at, id)` of the last row
/// examined. Kept between runs so rows that are still referenced do not
/// starve the ones behind them.
#[derive(Debug, Clone, Copy)]
struct IpCursor {
    updated_at: DateTime<Utc>,
    id: i32,
}

impl IpCursor {
    const START: IpCursor = IpCursor {
        updated_at: DateTime::<Utc>::UNIX_EPOCH,
        id: 0,
    };
}

#[derive(Debug, FromQueryResult)]
struct IpWindowRow {
    id: i32,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct IdRow {
    id: i32,
}

/// Outcome of one visitor delete statement.
#[derive(Debug)]
enum VisitorBatch {
    Deleted {
        deleted: u64,
        candidates: usize,
    },
    /// TimescaleDB refused the statement for decompressing too many tuples.
    OverLimit {
        ids: Vec<i32>,
    },
}

/// Whether `error` is TimescaleDB's decompression guard on a DML statement.
fn is_decompression_limit(error: &DbErr) -> bool {
    error
        .to_string()
        .contains("tuple decompression limit exceeded")
}

/// What the visitor sweep carries from one pass to the next.
#[derive(Debug, Default)]
struct VisitorSweepState {
    /// Visitors that exceeded the decompression limit on their own.
    skipped: Vec<i32>,
    passes: u32,
}

#[derive(Debug, FromQueryResult)]
struct ColumnRow {
    table_name: String,
    column_name: String,
}

#[derive(Debug, FromQueryResult)]
struct PolicySecondsRow {
    seconds: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct PresenceRow {
    present: bool,
}

pub struct AnalyticsRetentionService {
    db: Arc<DatabaseConnection>,
    ip_cursor: tokio::sync::Mutex<IpCursor>,
    visitor_sweep: tokio::sync::Mutex<VisitorSweepState>,
}

impl AnalyticsRetentionService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            ip_cursor: tokio::sync::Mutex::new(IpCursor::START),
            visitor_sweep: tokio::sync::Mutex::new(VisitorSweepState::default()),
        }
    }

    /// Apply every window in `settings`. A step that fails does not stop the
    /// others: its error is logged and the pass continues, so one broken
    /// table cannot hold up the rest of the cleanup.
    pub async fn enforce(&self, settings: &AnalyticsRetentionSettings) -> RetentionReport {
        let mut report = RetentionReport::default();
        let now = Utc::now();

        match self
            .sync_policy(EVENTS_TABLE, Some(settings.events_days))
            .await
        {
            Ok(action) => report.events_policy = Some(action),
            Err(error) => {
                warn!(table = EVENTS_TABLE, %error, "analytics retention policy sync failed")
            }
        }
        match self
            .sync_policy(ERROR_EVENTS_TABLE, settings.error_events_days)
            .await
        {
            Ok(action) => report.error_events_policy = Some(action),
            Err(error) => {
                warn!(table = ERROR_EVENTS_TABLE, %error, "analytics retention policy sync failed")
            }
        }

        // Visitors before addresses: a deleted visitor releases its address.
        if let Some(days) = settings.visitor_days {
            let cutoff = now - chrono::Duration::days(i64::from(days));
            match self.purge_visitors(cutoff).await {
                Ok(deleted) => report.visitors_deleted = deleted,
                Err(error) => warn!(retention_days = days, %error, "visitor retention pass failed"),
            }
        }
        if let Some(days) = settings.unreferenced_ip_days {
            let cutoff = now - chrono::Duration::days(i64::from(days));
            match self.purge_unreferenced_ips(cutoff).await {
                Ok(deleted) => report.ip_addresses_deleted = deleted,
                Err(error) => {
                    warn!(retention_days = days, %error, "IP address retention pass failed")
                }
            }
        }

        if report.visitors_deleted > 0 || report.ip_addresses_deleted > 0 {
            info!(
                visitors_deleted = report.visitors_deleted,
                ip_addresses_deleted = report.ip_addresses_deleted,
                "analytics retention removed expired rows"
            );
        }
        report
    }

    /// Bring the retention policy of `table` in step with `desired_days`.
    ///
    /// A deployment without TimescaleDB (or where the table is not a
    /// hypertable) has no policy to manage and is left alone.
    pub async fn sync_policy(
        &self,
        table: &'static str,
        desired_days: Option<u32>,
    ) -> Result<PolicyAction, AnalyticsRetentionError> {
        if !self.is_hypertable(table).await? {
            debug!(
                table,
                "not a TimescaleDB hypertable; no retention policy to manage"
            );
            return Ok(PolicyAction::Keep);
        }
        let installed = self.installed_retention_days(table).await?;
        let action = policy_action(installed, desired_days);
        let sql = match action {
            PolicyAction::Keep => return Ok(action),
            PolicyAction::Set(days) => format!(
                "SELECT remove_retention_policy('{table}', if_exists => TRUE); \
                 SELECT add_retention_policy('{table}', \
                     drop_after => make_interval(days => {days}), if_not_exists => TRUE)"
            ),
            PolicyAction::Remove => {
                format!("SELECT remove_retention_policy('{table}', if_exists => TRUE)")
            }
        };
        self.db.execute_unprepared(&sql).await.map_err(|source| {
            AnalyticsRetentionError::database(
                format!("set the retention policy of {table} to {action:?}"),
                source,
            )
        })?;
        info!(
            table,
            installed_days = ?installed,
            ?action,
            "analytics retention policy updated"
        );
        Ok(action)
    }

    async fn is_hypertable(&self, table: &str) -> Result<bool, AnalyticsRetentionError> {
        let presence = PresenceRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT (to_regclass('timescaledb_information.hypertables') IS NOT NULL \
                     AND EXISTS (SELECT 1 FROM timescaledb_information.hypertables \
                                 WHERE hypertable_schema = current_schema() \
                                   AND hypertable_name = $1)) AS present",
            [table.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| {
            AnalyticsRetentionError::database(
                format!("check whether {table} is a hypertable"),
                source,
            )
        })?;
        Ok(presence.is_some_and(|row| row.present))
    }

    async fn installed_retention_days(
        &self,
        table: &str,
    ) -> Result<Option<u32>, AnalyticsRetentionError> {
        let row = PolicySecondsRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT EXTRACT(EPOCH FROM (config ->> 'drop_after')::interval)::BIGINT AS seconds \
             FROM timescaledb_information.jobs \
             WHERE proc_name = 'policy_retention' \
               AND hypertable_schema = current_schema() \
               AND hypertable_name = $1 \
             LIMIT 1",
            [table.into()],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| {
            AnalyticsRetentionError::database(
                format!("read the retention policy of {table}"),
                source,
            )
        })?;
        Ok(row
            .and_then(|row| row.seconds)
            .filter(|seconds| *seconds > 0)
            .and_then(|seconds| u32::try_from((seconds + 43_200) / 86_400).ok()))
    }

    /// Delete visitors last seen before `cutoff`, at most
    /// `BATCH_SIZE * MAX_BATCHES_PER_RUN` per call. Returns the rows deleted.
    ///
    /// Each batch is one transaction. Three things about it are deliberate:
    ///
    /// - The `DELETE` repeats the age test. The candidate ids were read in
    ///   their own statement, so a visitor that returned (ingestion advanced
    ///   `last_seen`) in between would otherwise still be deleted; PostgreSQL
    ///   re-evaluates the *target* row's predicate after waiting for the
    ///   concurrent update, so with it the refreshed visitor is kept.
    /// - Deleting a visitor rewrites every `error_events`/`events`/`proxy_logs`
    ///   row that points at it (`ON DELETE SET NULL`). Those hypertables are
    ///   compressed, and TimescaleDB aborts a statement that has to decompress
    ///   more than `max_tuples_decompressed_per_dml_transaction` tuples (100 000
    ///   by default) -- which would fail this batch, and the same oldest batch
    ///   on every later pass. The limit is raised to [`DECOMPRESSION_LIMIT`]
    ///   for this transaction only, never removed, so memory stays bounded.
    /// - A batch that still hits the limit is retried at half the size. A
    ///   single visitor that exceeds it on its own is left in place and
    ///   remembered across passes (retried daily), so it neither fails the
    ///   pass nor sits in front of everything behind it forever.
    pub async fn purge_visitors(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, AnalyticsRetentionError> {
        let mut total = 0u64;
        let mut batch_size = BATCH_SIZE;
        // Held for the whole pass, like the IP cursor: only one pass runs.
        let mut state = self.visitor_sweep.lock().await;
        // Oversized visitors would otherwise sit at the front of the oldest-
        // first scan on every pass and starve everything behind them. They are
        // remembered between passes and retried now and then, in case the
        // server limit was raised or the rows they own have been removed.
        state.passes = state.passes.wrapping_add(1);
        if state.passes % OVERSIZED_RETRY_EVERY_PASSES == 0 {
            state.skipped.clear();
        }

        for statement in 0..MAX_BATCHES_PER_RUN {
            match self
                .purge_visitor_batch(cutoff, batch_size, &state.skipped)
                .await?
            {
                VisitorBatch::Deleted {
                    deleted,
                    candidates,
                } => {
                    total += deleted;
                    if (candidates as i64) < batch_size {
                        break;
                    }
                    // A full batch went through: grow back toward the normal size.
                    batch_size = (batch_size * 2).min(BATCH_SIZE);
                }
                VisitorBatch::OverLimit { ids } if ids.len() == 1 => {
                    warn!(
                        visitor_id = ids[0],
                        limit = DECOMPRESSION_LIMIT,
                        "visitor has more related rows than the decompression limit allows \
                         in one transaction; leaving it in place and moving on"
                    );
                    state.skipped.extend(ids);
                    if state.skipped.len() >= MAX_OVERSIZED_VISITORS {
                        warn!(
                            skipped = state.skipped.len(),
                            "ending the visitor retention pass: too many visitors exceed the \
                             decompression limit; raise \
                             timescaledb.max_tuples_decompressed_per_dml_transaction"
                        );
                        break;
                    }
                }
                VisitorBatch::OverLimit { ids } => {
                    batch_size = (ids.len() as i64 / 2).max(1);
                    debug!(
                        batch_size,
                        "visitor batch exceeded the decompression limit; retrying smaller"
                    );
                }
            }
            if statement + 1 < MAX_BATCHES_PER_RUN {
                tokio::time::sleep(BATCH_PAUSE).await;
            }
        }
        Ok(total)
    }

    async fn purge_visitor_batch(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: i64,
        skip: &[i32],
    ) -> Result<VisitorBatch, AnalyticsRetentionError> {
        let operation = || format!("delete visitors last seen before {cutoff}");
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| AnalyticsRetentionError::database(operation(), source))?;

        // `set_config(.., true)` is `SET LOCAL`. Guarded by `pg_settings` so
        // that a server without the setting (older TimescaleDB, or none) is
        // left alone instead of failing on an unknown parameter.
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT set_config('timescaledb.max_tuples_decompressed_per_dml_transaction', $1, true) \
             WHERE EXISTS (SELECT 1 FROM pg_settings \
                           WHERE name = 'timescaledb.max_tuples_decompressed_per_dml_transaction')",
            [DECOMPRESSION_LIMIT.to_string().into()],
        ))
        .await
        .map_err(|source| AnalyticsRetentionError::database(operation(), source))?;

        let ids: Vec<i32> = IdRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM visitor WHERE last_seen < $1 AND id <> ALL($3::int4[]) \
             ORDER BY last_seen ASC LIMIT $2",
            [cutoff.into(), batch_size.into(), skip.to_vec().into()],
        ))
        .all(&txn)
        .await
        .map_err(|source| AnalyticsRetentionError::database(operation(), source))?
        .into_iter()
        .map(|row| row.id)
        .collect();
        if ids.is_empty() {
            txn.commit()
                .await
                .map_err(|source| AnalyticsRetentionError::database(operation(), source))?;
            return Ok(VisitorBatch::Deleted {
                deleted: 0,
                candidates: 0,
            });
        }

        let result = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM visitor WHERE last_seen < $1 AND id = ANY($2::int4[])",
                [cutoff.into(), ids.clone().into()],
            ))
            .await;
        match result {
            Ok(result) => {
                txn.commit()
                    .await
                    .map_err(|source| AnalyticsRetentionError::database(operation(), source))?;
                Ok(VisitorBatch::Deleted {
                    deleted: result.rows_affected(),
                    candidates: ids.len(),
                })
            }
            Err(source) if is_decompression_limit(&source) => {
                // The transaction is aborted; dropping it rolls it back.
                drop(txn);
                Ok(VisitorBatch::OverLimit { ids })
            }
            Err(source) => Err(AnalyticsRetentionError::database(operation(), source)),
        }
    }

    /// Delete stored IP addresses that nothing references and that were last
    /// looked up before `cutoff`. Returns the rows deleted.
    pub async fn purge_unreferenced_ips(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, AnalyticsRetentionError> {
        let references = self.existing_reference_columns().await?;
        let mut cursor = self.ip_cursor.lock().await;
        let mut total = 0u64;

        for batch in 0..MAX_BATCHES_PER_RUN {
            let window = IpWindowRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, updated_at FROM ip_geolocations \
                 WHERE updated_at < $1 AND COALESCE(last_used_at, updated_at) < $1 \
                   AND (updated_at, id) > ($2, $3) \
                 ORDER BY updated_at ASC, id ASC LIMIT $4",
                [
                    cutoff.into(),
                    cursor.updated_at.into(),
                    cursor.id.into(),
                    BATCH_SIZE.into(),
                ],
            ))
            .all(self.db.as_ref())
            .await
            .map_err(|source| {
                AnalyticsRetentionError::database(
                    format!("list IP addresses not looked up since {cutoff}"),
                    source,
                )
            })?;

            let Some(last) = window.last() else {
                // End of the candidates: start over next run.
                *cursor = IpCursor::START;
                break;
            };
            let next = IpCursor {
                updated_at: last.updated_at,
                id: last.id,
            };
            let ids: Vec<i32> = window.iter().map(|row| row.id).collect();
            let reached_end = (ids.len() as i64) < BATCH_SIZE;

            let deleted = self
                .db
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    delete_unreferenced_sql(&ids, &references),
                    [cutoff.into()],
                ))
                .await
                .map_err(|source| {
                    AnalyticsRetentionError::database(
                        format!("delete {} unreferenced IP addresses", ids.len()),
                        source,
                    )
                })?;
            total += deleted.len() as u64;
            *cursor = next;

            if reached_end {
                *cursor = IpCursor::START;
                break;
            }
            if batch + 1 < MAX_BATCHES_PER_RUN {
                tokio::time::sleep(BATCH_PAUSE).await;
            }
        }
        Ok(total)
    }

    /// The subset of [`IP_REFERENCES`] present in this database.
    ///
    /// Introspected rather than assumed: a missing table must not turn the
    /// whole cleanup into an error, and a column that exists must always be
    /// consulted.
    async fn existing_reference_columns(
        &self,
    ) -> Result<Vec<(&'static str, &'static str)>, AnalyticsRetentionError> {
        let rows = ColumnRow::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT table_name, column_name FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND ({})",
                IP_REFERENCES
                    .iter()
                    .map(|(table, column)| format!(
                        "(table_name = '{table}' AND column_name = '{column}')"
                    ))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ),
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| {
            AnalyticsRetentionError::database(
                "list the columns that reference IP addresses",
                source,
            )
        })?;

        Ok(IP_REFERENCES
            .iter()
            .copied()
            .filter(|(table, column)| {
                rows.iter()
                    .any(|row| row.table_name == *table && row.column_name == *column)
            })
            .collect())
    }
}

/// `DELETE` of the candidate ids that no referencing column points at and
/// that no writer has resolved since `$1` (the cutoff), returning the ids
/// actually removed.
///
/// The cutoff is tested again on the target row, not only when the candidates
/// were listed: `IpAddressService` advances `last_used_at` when it hands an id
/// to a writer, and the re-check is what makes a delete that races that
/// update lose to it.
///
/// The ids are `i32`s read from the database a moment earlier and the table
/// and column names are constants, so the statement is built directly: there
/// is no caller-controlled text in it.
fn delete_unreferenced_sql(ids: &[i32], references: &[(&str, &str)]) -> String {
    let id_list = ids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let not_referenced: String = references
        .iter()
        .map(|(table, column)| {
            format!(" AND NOT EXISTS (SELECT 1 FROM {table} r WHERE r.{column} = g.id)")
        })
        .collect();
    format!(
        "DELETE FROM ip_geolocations g WHERE g.id IN ({id_list}) \
         AND COALESCE(g.last_used_at, g.updated_at) < $1{not_referenced} RETURNING g.id"
    )
}

/// The windows to enforce, given the proxy-log retention in force.
///
/// Request logs may be stored outside PostgreSQL (ClickHouse), where they carry
/// an `ip_geolocation_id` that [`IP_REFERENCES`] cannot see. `last_used_at`
/// advances whenever such a log is written, so keeping an address for at least
/// the proxy-log window after its last lookup keeps it for as long as any of
/// those logs can still point at it. The floor is the same whichever store is
/// in use, so it needs no knowledge of which one that is.
///
/// Logs keep the window they were written with, so after the window is lowered
/// the older logs outlive the new floor: while `ip_hold_until` is in the
/// future no address is deleted at all.
pub fn effective_windows(
    analytics: &AnalyticsRetentionSettings,
    proxy_logs_days: u32,
    now: DateTime<Utc>,
) -> AnalyticsRetentionSettings {
    let held = analytics.ip_hold_until.is_some_and(|until| until > now);
    AnalyticsRetentionSettings {
        unreferenced_ip_days: if held {
            None
        } else {
            analytics
                .unreferenced_ip_days
                .map(|days| days.max(proxy_logs_days))
        },
        ..analytics.clone()
    }
}

/// Audit record of one retention pass that changed something. The actor is the
/// system, not a user, so no user id is recorded. One entry per pass, never one
/// per row: the counts and windows are what an operator needs to answer "what
/// deleted my data, and under which setting".
#[derive(Debug, Clone, Serialize)]
pub struct RetentionSweepAudit {
    pub events_policy: Option<String>,
    pub error_events_policy: Option<String>,
    pub visitor_days: Option<u32>,
    pub visitors_deleted: u64,
    pub unreferenced_ip_days: Option<u32>,
    pub ip_addresses_deleted: u64,
}

impl RetentionSweepAudit {
    /// `None` when the pass changed nothing, so an idle hour writes no entry.
    pub fn from_report(
        report: &RetentionReport,
        windows: &AnalyticsRetentionSettings,
    ) -> Option<Self> {
        let changed =
            |action: Option<PolicyAction>| action.filter(|action| *action != PolicyAction::Keep);
        let events_policy = changed(report.events_policy);
        let error_events_policy = changed(report.error_events_policy);
        if events_policy.is_none()
            && error_events_policy.is_none()
            && report.visitors_deleted == 0
            && report.ip_addresses_deleted == 0
        {
            return None;
        }
        Some(Self {
            events_policy: events_policy.map(|action| format!("{action:?}")),
            error_events_policy: error_events_policy.map(|action| format!("{action:?}")),
            visitor_days: windows.visitor_days,
            visitors_deleted: report.visitors_deleted,
            unreferenced_ip_days: windows.unreferenced_ip_days,
            ip_addresses_deleted: report.ip_addresses_deleted,
        })
    }
}

impl AuditOperation for RetentionSweepAudit {
    fn operation_type(&self) -> String {
        "ANALYTICS_RETENTION_APPLIED".to_string()
    }

    /// The sweep runs as the system: there is no user account to attribute it to.
    fn user_id(&self) -> Option<i32> {
        None
    }

    fn ip_address(&self) -> Option<String> {
        None
    }

    fn user_agent(&self) -> &str {
        "temps-analytics-retention"
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize ANALYTICS_RETENTION_APPLIED audit: {}",
                e
            )
        })
    }
}

/// Spawn the hourly enforcement loop. The settings are re-read every pass, so
/// a change in the console applies within [`RETENTION_INTERVAL`].
pub fn spawn_retention_job(
    service: Arc<AnalyticsRetentionService>,
    config: Arc<temps_config::ConfigService>,
    audit: Arc<dyn AuditLogger>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(RETENTION_STARTUP_DELAY).await;
        let mut interval = tokio::time::interval(RETENTION_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match config.get_settings().await {
                Ok(settings) => {
                    let windows = effective_windows(
                        &settings.analytics_retention,
                        settings.observability_retention.proxy_logs_days,
                        Utc::now(),
                    );
                    let report = service.enforce(&windows).await;
                    if let Some(entry) = RetentionSweepAudit::from_report(&report, &windows) {
                        // The data is already gone; a failed audit write is
                        // logged, not retried, and does not stop the job.
                        if let Err(error) = audit.create_audit_log(&entry).await {
                            warn!(%error, "failed to audit the analytics retention pass");
                        }
                    }
                }
                Err(error) => {
                    let error = AnalyticsRetentionError::Settings {
                        reason: error.to_string(),
                    };
                    warn!(%error, "skipping analytics retention pass");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn mock_service(db: DatabaseConnection) -> AnalyticsRetentionService {
        AnalyticsRetentionService::new(Arc::new(db))
    }

    #[test]
    fn ip_window_is_never_shorter_than_the_proxy_log_window() {
        let settings = AnalyticsRetentionSettings {
            unreferenced_ip_days: Some(30),
            ..AnalyticsRetentionSettings::default()
        };

        assert_eq!(
            effective_windows(&settings, 90, Utc::now()).unreferenced_ip_days,
            Some(90),
            "logs kept for 90 days can still point at the address"
        );
        assert_eq!(
            effective_windows(&settings, 7, Utc::now()).unreferenced_ip_days,
            Some(30),
            "a longer IP window is left alone"
        );
    }

    #[test]
    fn a_hold_after_lowering_the_proxy_log_window_pauses_the_ip_sweep() {
        let now = Utc::now();
        let settings = AnalyticsRetentionSettings {
            unreferenced_ip_days: Some(30),
            ip_hold_until: Some(now + chrono::Duration::days(80)),
            ..AnalyticsRetentionSettings::default()
        };

        assert_eq!(
            effective_windows(&settings, 7, now).unreferenced_ip_days,
            None,
            "logs written under the old window can still point at the address"
        );
        assert_eq!(
            effective_windows(&settings, 7, now + chrono::Duration::days(81)).unreferenced_ip_days,
            Some(30),
            "the sweep resumes once the hold has passed"
        );
    }

    #[test]
    fn an_idle_pass_writes_no_audit_entry() {
        let windows = AnalyticsRetentionSettings::default();

        assert!(RetentionSweepAudit::from_report(&RetentionReport::default(), &windows).is_none());
        let kept = RetentionReport {
            events_policy: Some(PolicyAction::Keep),
            ..RetentionReport::default()
        };
        assert!(RetentionSweepAudit::from_report(&kept, &windows).is_none());
    }

    #[test]
    fn a_pass_that_deleted_rows_is_audited_with_its_windows_and_counts() {
        let windows = AnalyticsRetentionSettings {
            visitor_days: Some(425),
            ..AnalyticsRetentionSettings::default()
        };
        let report = RetentionReport {
            events_policy: Some(PolicyAction::Set(90)),
            visitors_deleted: 12,
            ip_addresses_deleted: 3,
            ..RetentionReport::default()
        };

        let entry = RetentionSweepAudit::from_report(&report, &windows).expect("audited");

        assert_eq!(entry.user_id(), None);
        assert_eq!(entry.operation_type(), "ANALYTICS_RETENTION_APPLIED");
        let json = AuditOperation::serialize(&entry).expect("serializes");
        assert!(json.contains("\"visitors_deleted\":12"), "{json}");
        assert!(json.contains("\"visitor_days\":425"), "{json}");
        assert!(json.contains("Set(90)"), "{json}");
    }

    #[test]
    fn keeping_ip_addresses_forever_stays_forever() {
        let settings = AnalyticsRetentionSettings {
            unreferenced_ip_days: None,
            ..AnalyticsRetentionSettings::default()
        };

        let windows = effective_windows(&settings, 90, Utc::now());

        assert_eq!(windows.unreferenced_ip_days, None);
        assert_eq!(windows.events_days, settings.events_days);
    }

    #[test]
    fn policy_action_keeps_a_matching_policy() {
        assert_eq!(policy_action(Some(90), Some(90)), PolicyAction::Keep);
        assert_eq!(policy_action(None, None), PolicyAction::Keep);
    }

    #[test]
    fn policy_action_installs_or_replaces_to_the_configured_window() {
        assert_eq!(policy_action(None, Some(30)), PolicyAction::Set(30));
        assert_eq!(policy_action(Some(90), Some(30)), PolicyAction::Set(30));
    }

    #[test]
    fn policy_action_removes_a_policy_when_the_window_becomes_keep() {
        assert_eq!(policy_action(Some(30), None), PolicyAction::Remove);
    }

    #[test]
    fn delete_sql_checks_every_reference_and_returns_the_ids() {
        let sql = delete_unreferenced_sql(
            &[3, 5],
            &[
                ("events", "ip_geolocation_id"),
                ("visitor", "ip_address_id"),
            ],
        );
        assert!(sql.starts_with("DELETE FROM ip_geolocations g WHERE g.id IN (3, 5)"));
        assert!(
            sql.contains("COALESCE(g.last_used_at, g.updated_at) < $1"),
            "the target row is re-checked against the cutoff: {sql}"
        );
        assert!(
            sql.contains("NOT EXISTS (SELECT 1 FROM events r WHERE r.ip_geolocation_id = g.id)")
        );
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM visitor r WHERE r.ip_address_id = g.id)"));
        assert!(sql.ends_with("RETURNING g.id"));
    }

    #[test]
    fn reference_list_covers_audit_logs_and_visitors() {
        // The columns without a foreign key are the dangerous ones: the
        // database would not stop a delete, so dropping one of these from the
        // list silently orphans live rows.
        for expected in [
            ("audit_logs", "ip_address_id"),
            ("visitor", "ip_address_id"),
            ("performance_metrics", "ip_address_id"),
            ("proxy_logs", "ip_geolocation_id"),
            ("events", "ip_geolocation_id"),
            ("error_events", "ip_geolocation_id"),
        ] {
            assert!(IP_REFERENCES.contains(&expected), "missing {expected:?}");
        }
    }

    fn exec_ok(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    fn id_rows(
        ids: impl IntoIterator<Item = i32>,
    ) -> Vec<std::collections::BTreeMap<&'static str, sea_orm::Value>> {
        ids.into_iter()
            .map(|id| std::collections::BTreeMap::from([("id", sea_orm::Value::Int(Some(id)))]))
            .collect()
    }

    fn decompression_limit_error() -> DbErr {
        DbErr::Custom("tuple decompression limit exceeded by operation".into())
    }

    #[tokio::test]
    async fn purge_visitors_stops_after_a_short_batch() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([id_rows(1..=7)])
            // set_config, then the DELETE
            .append_exec_results([exec_ok(0), exec_ok(7)])
            .into_connection();
        let service = mock_service(db);

        let deleted = service
            .purge_visitors(Utc::now())
            .await
            .expect("a short batch ends the pass");

        assert_eq!(deleted, 7);
    }

    #[tokio::test]
    async fn purge_visitors_binds_cutoff_and_batch_size() {
        let cutoff = Utc::now() - chrono::Duration::days(425);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([id_rows([9])])
                .append_exec_results([exec_ok(0), exec_ok(1)])
                .into_connection(),
        );
        let service = AnalyticsRetentionService::new(db.clone());
        service.purge_visitors(cutoff).await.expect("one visitor");
        drop(service);

        let log = Arc::try_unwrap(db)
            .unwrap_or_else(|_| panic!("service dropped, connection is unshared"))
            .into_transaction_log();
        assert_eq!(log.len(), 1, "one transaction per batch");
        let rendered = format!("{log:?}");
        assert!(
            rendered.contains("DELETE FROM visitor WHERE last_seen < $1 AND id = ANY($2"),
            "the target row repeats the age test: {rendered}"
        );
        assert!(rendered.contains("LIMIT $2"), "{rendered}");
        assert!(
            rendered.contains("max_tuples_decompressed_per_dml_transaction"),
            "the decompression limit is raised for the batch: {rendered}"
        );
        assert!(
            rendered.contains(&DECOMPRESSION_LIMIT.to_string()),
            "the limit is finite, not unlimited: {rendered}"
        );
    }

    #[tokio::test]
    async fn purge_visitors_reports_the_failed_operation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([id_rows([1])])
            .append_exec_results([exec_ok(0)])
            .append_exec_errors([DbErr::Custom("deadlock detected".into())])
            .into_connection();
        let service = mock_service(db);

        let error = service
            .purge_visitors(Utc::now())
            .await
            .expect_err("a database failure surfaces");

        let message = error.to_string();
        assert!(
            message.contains("delete visitors last seen before"),
            "{message}"
        );
        assert!(message.contains("deadlock detected"), "{message}");
    }

    #[tokio::test]
    async fn a_batch_over_the_decompression_limit_is_retried_smaller() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // First attempt: a full batch that the server refuses. Second: the
            // halved batch, which is short and finishes the pass.
            .append_query_results([id_rows(1..=BATCH_SIZE as i32), id_rows(1..=3)])
            .append_exec_results([exec_ok(0)])
            .append_exec_errors([decompression_limit_error()])
            .append_exec_results([exec_ok(0), exec_ok(3)])
            .into_connection();
        let service = mock_service(db);

        let deleted = service
            .purge_visitors(Utc::now())
            .await
            .expect("the limit is handled, not surfaced");

        assert_eq!(deleted, 3);
    }

    #[tokio::test]
    async fn one_visitor_over_the_limit_is_skipped_not_fatal() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // The single candidate is refused; the next selection (which
            // excludes it) is empty, so the pass ends cleanly.
            .append_query_results([id_rows([5]), id_rows([])])
            .append_exec_results([exec_ok(0)])
            .append_exec_errors([decompression_limit_error()])
            .append_exec_results([exec_ok(0)])
            .into_connection();
        let service = mock_service(db);

        let deleted = service
            .purge_visitors(Utc::now())
            .await
            .expect("an oversized visitor must not fail the pass");

        assert_eq!(deleted, 0);
    }

    #[tokio::test]
    async fn an_oversized_visitor_is_remembered_so_the_next_pass_moves_past_it() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // Pass 1: visitor 5 is refused alone, then nothing is left.
                // Pass 2: the scan runs again and must exclude visitor 5.
                .append_query_results([id_rows([5]), id_rows([]), id_rows([])])
                .append_exec_results([exec_ok(0)])
                .append_exec_errors([decompression_limit_error()])
                .append_exec_results([exec_ok(0), exec_ok(0)])
                .into_connection(),
        );
        let service = AnalyticsRetentionService::new(db.clone());
        service.purge_visitors(Utc::now()).await.expect("pass 1");
        service.purge_visitors(Utc::now()).await.expect("pass 2");
        drop(service);

        let log = Arc::try_unwrap(db)
            .unwrap_or_else(|_| panic!("service dropped, connection is unshared"))
            .into_transaction_log();
        let rendered: Vec<String> = log.iter().map(|t| format!("{t:?}")).collect();
        let last = rendered.last().expect("a second pass ran");
        assert!(
            last.contains("id <> ALL") && last.contains("Some([Int(Some(5))])"),
            "the second pass excludes the oversized visitor: {last}"
        );
    }

    #[tokio::test]
    async fn many_oversized_visitors_do_not_end_the_pass_before_later_ones() {
        // More than the old per-pass limit of skipped visitors, followed by one
        // that deletes normally. The pass must reach it.
        let oversized = 21;
        let mut db = MockDatabase::new(DatabaseBackend::Postgres);
        for id in 1..=oversized {
            db = db
                .append_query_results([id_rows([id])])
                .append_exec_results([exec_ok(0)])
                .append_exec_errors([decompression_limit_error()]);
        }
        let db = db
            .append_query_results([id_rows([900])])
            .append_exec_results([exec_ok(0), exec_ok(1)])
            .into_connection();
        let service = mock_service(db);

        let deleted = service
            .purge_visitors(Utc::now())
            .await
            .expect("oversized visitors do not fail the pass");

        assert_eq!(deleted, 1);
    }

    #[test]
    fn only_the_decompression_guard_is_treated_as_a_limit() {
        assert!(is_decompression_limit(&decompression_limit_error()));
        assert!(!is_decompression_limit(&DbErr::Custom(
            "deadlock detected".into()
        )));
    }

    #[tokio::test]
    async fn sync_policy_leaves_a_non_hypertable_alone() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[std::collections::BTreeMap::from([(
                "present",
                sea_orm::Value::Bool(Some(false)),
            )])]])
            .into_connection();
        let service = mock_service(db);

        let action = service
            .sync_policy(ERROR_EVENTS_TABLE, Some(30))
            .await
            .expect("a plain table has no policy to manage");

        assert_eq!(action, PolicyAction::Keep);
    }
}

#[cfg(test)]
mod integration_tests {
    //! Run against a real TimescaleDB container; skipped without Docker.

    use super::*;
    use crate::testing::test_utils::TestDatabase;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};
    use temps_entities::upstream_config::UpstreamList;
    use temps_entities::{audit_logs, environments, ip_geolocations, projects, visitor};

    /// Only an unreachable Docker daemon skips a test. Once Docker answers, a
    /// failure to start the container or to run the migrations is a broken
    /// setup and fails the test: skipping it would leave every integration
    /// test passing without checking anything.
    async fn database(name: &str) -> Option<TestDatabase> {
        let docker_answers = match bollard::Docker::connect_with_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        };
        if !docker_answers {
            println!("Docker not available, skipping");
            return None;
        }
        match TestDatabase::new(name).await {
            Ok(db) => Some(db),
            Err(error) => panic!("test database setup failed with Docker available: {error:#}"),
        }
    }

    /// Insert an address row last looked up `age_days` ago.
    async fn insert_ip(db: &DatabaseConnection, ip: &str, age_days: i64) -> i32 {
        let at = Utc::now() - chrono::Duration::days(age_days);
        ip_geolocations::ActiveModel {
            ip_address: Set(ip.to_string()),
            country: Set("Testland".to_string()),
            is_eu: Set(false),
            created_at: Set(at),
            updated_at: Set(at),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert ip")
        .id
    }

    async fn ip_exists(db: &DatabaseConnection, id: i32) -> bool {
        ip_geolocations::Entity::find_by_id(id)
            .one(db)
            .await
            .expect("query ip")
            .is_some()
    }

    /// A project with one environment; returns `(project_id, environment_id)`.
    async fn seed_environment(db: &DatabaseConnection) -> (i32, i32) {
        let now = Utc::now();
        let project = projects::ActiveModel {
            name: Set("retention".to_string()),
            slug: Set("retention".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            repo_name: Set("retention".to_string()),
            repo_owner: Set("retention".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert project");
        let environment = environments::ActiveModel {
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("retention".to_string()),
            host: Set("retention.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            project_id: Set(project.id),
            branch: Set(Some("main".to_string())),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert environment");
        (project.id, environment.id)
    }

    async fn insert_visitor(
        db: &DatabaseConnection,
        ids: (i32, i32),
        name: &str,
        seen_days_ago: i64,
        ip_address_id: Option<i32>,
    ) {
        let seen = Utc::now() - chrono::Duration::days(seen_days_ago);
        visitor::ActiveModel {
            visitor_id: Set(name.to_string()),
            project_id: Set(ids.0),
            environment_id: Set(ids.1),
            first_seen: Set(seen),
            last_seen: Set(seen),
            ip_address_id: Set(ip_address_id),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert visitor");
    }

    #[tokio::test]
    async fn only_unreferenced_and_old_addresses_are_deleted() {
        let Some(test_db) = database("retention_ips").await else {
            return;
        };
        let db = test_db.db.as_ref();

        let stale_orphan = insert_ip(db, "198.51.100.1", 60).await;
        let recent_orphan = insert_ip(db, "198.51.100.2", 2).await;
        let used_by_visitor = insert_ip(db, "198.51.100.3", 60).await;
        let used_by_audit = insert_ip(db, "198.51.100.4", 60).await;

        let ids = seed_environment(db).await;
        insert_visitor(db, ids, "kept-visitor", 1, Some(used_by_visitor)).await;
        let now = Utc::now();
        audit_logs::ActiveModel {
            user_agent: Set("retention-test".to_string()),
            operation_type: Set("LOGIN".to_string()),
            ip_address_id: Set(Some(used_by_audit)),
            audit_date: Set(now),
            created_at: Set(now),
            data: Set("{}".to_string()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert audit log");

        let service = AnalyticsRetentionService::new(test_db.db.clone());
        let deleted = service
            .purge_unreferenced_ips(Utc::now() - chrono::Duration::days(30))
            .await
            .expect("purge runs");

        assert_eq!(deleted, 1);
        assert!(
            !ip_exists(db, stale_orphan).await,
            "stale orphan is removed"
        );
        assert!(
            ip_exists(db, recent_orphan).await,
            "recent lookups are kept"
        );
        assert!(
            ip_exists(db, used_by_visitor).await,
            "visitor still uses it"
        );
        assert!(
            ip_exists(db, used_by_audit).await,
            "audit log still uses it"
        );
    }

    #[tokio::test]
    async fn an_address_resolved_recently_is_kept_even_when_unreferenced() {
        let Some(test_db) = database("retention_ip_in_use").await else {
            return;
        };
        let db = test_db.db.as_ref();

        // Verified long ago (`updated_at`), but a writer resolved it an hour
        // ago and may be about to store, or already caches, its id.
        let in_use = insert_ip(db, "198.51.100.20", 400).await;
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE ip_geolocations SET last_used_at = now() - interval '1 hour' WHERE id = $1",
            [in_use.into()],
        ))
        .await
        .expect("record the lookup");
        let idle = insert_ip(db, "198.51.100.21", 400).await;

        let service = AnalyticsRetentionService::new(test_db.db.clone());
        let deleted = service
            .purge_unreferenced_ips(Utc::now() - chrono::Duration::days(30))
            .await
            .expect("purge runs");

        assert_eq!(deleted, 1);
        assert!(ip_exists(db, in_use).await, "a recently resolved id stays");
        assert!(!ip_exists(db, idle).await, "an idle one is removed");
    }

    #[tokio::test]
    async fn a_visitor_that_returns_mid_sweep_is_not_deleted() {
        let Some(test_db) = database("retention_visitor_race").await else {
            return;
        };
        let db = test_db.db.clone();
        let ids = seed_environment(db.as_ref()).await;
        insert_visitor(db.as_ref(), ids, "returning", 480, None).await;

        // Ingestion refreshes the visitor in a transaction that is still
        // open when the sweep starts, so the sweep's candidate list was read
        // from the old row version.
        let refresh = db.begin().await.expect("begin");
        refresh
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "UPDATE visitor SET last_seen = now() WHERE visitor_id = 'returning'",
            ))
            .await
            .expect("refresh the visitor");

        let service = Arc::new(AnalyticsRetentionService::new(db.clone()));
        let sweep = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .purge_visitors(Utc::now() - chrono::Duration::days(425))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
        refresh.commit().await.expect("commit the refresh");

        let deleted = sweep.await.expect("join").expect("purge runs");
        assert_eq!(deleted, 0, "the refreshed visitor must survive");
        assert_eq!(
            visitor::Entity::find()
                .all(db.as_ref())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn an_address_is_released_once_its_visitor_is_purged() {
        let Some(test_db) = database("retention_chain").await else {
            return;
        };
        let db = test_db.db.as_ref();
        let ip = insert_ip(db, "198.51.100.9", 500).await;
        let ids = seed_environment(db).await;
        insert_visitor(db, ids, "gone", 480, Some(ip)).await;

        let service = AnalyticsRetentionService::new(test_db.db.clone());
        let report = service
            .enforce(&AnalyticsRetentionSettings {
                events_days: 90,
                error_events_days: None,
                visitor_days: Some(425),
                unreferenced_ip_days: Some(30),
                ip_hold_until: None,
            })
            .await;

        assert_eq!(report.visitors_deleted, 1);
        assert_eq!(report.ip_addresses_deleted, 1);
        assert!(!ip_exists(db, ip).await);
    }

    #[tokio::test]
    async fn visitors_not_seen_since_the_cutoff_are_deleted() {
        let Some(test_db) = database("retention_visitors").await else {
            return;
        };
        let db = test_db.db.as_ref();
        let ids = seed_environment(db).await;
        insert_visitor(db, ids, "old", 480, None).await;
        insert_visitor(db, ids, "recent", 3, None).await;

        let service = AnalyticsRetentionService::new(test_db.db.clone());
        let deleted = service
            .purge_visitors(Utc::now() - chrono::Duration::days(425))
            .await
            .expect("purge runs");

        assert_eq!(deleted, 1);
        let remaining = visitor::Entity::find()
            .all(db)
            .await
            .expect("remaining visitors");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].visitor_id, "recent");
    }

    #[tokio::test]
    async fn the_error_events_policy_follows_the_setting() {
        let Some(test_db) = database("retention_policy").await else {
            return;
        };
        let service = AnalyticsRetentionService::new(test_db.db.clone());

        // `events` ships with the 90-day policy from the initial schema.
        assert_eq!(
            service.sync_policy(EVENTS_TABLE, Some(90)).await.unwrap(),
            PolicyAction::Keep
        );
        assert_eq!(
            service.sync_policy(EVENTS_TABLE, Some(30)).await.unwrap(),
            PolicyAction::Set(30)
        );
        assert_eq!(
            service.sync_policy(EVENTS_TABLE, Some(30)).await.unwrap(),
            PolicyAction::Keep,
            "an unchanged setting is a no-op on the next pass"
        );

        // `error_events` has no policy until one is configured.
        assert_eq!(
            service.sync_policy(ERROR_EVENTS_TABLE, None).await.unwrap(),
            PolicyAction::Keep
        );
        assert_eq!(
            service
                .sync_policy(ERROR_EVENTS_TABLE, Some(14))
                .await
                .unwrap(),
            PolicyAction::Set(14)
        );
        assert_eq!(
            service.sync_policy(ERROR_EVENTS_TABLE, None).await.unwrap(),
            PolicyAction::Remove
        );
    }
}
