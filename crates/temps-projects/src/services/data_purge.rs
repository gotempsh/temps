// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Background removal of a deleted project's telemetry.
//!
//! The high-volume telemetry tables ([`PURGED_TABLES`]) hold no foreign key to
//! `projects`, so deleting a project no longer cascades into them: that
//! cascade scanned every request and analytics row once per deployment and
//! failed outright on compressed chunks. Instead, deleting a project records a
//! `project_data_purges` row in the same transaction (see
//! [`ProjectDataPurger::enqueue`]), and this worker removes the rows in the
//! background:
//!
//! - **Hypertables, chunk by chunk.** A compressed chunk is purged with one
//!   `DELETE ... WHERE project_id = $1` against the chunk itself. `project_id`
//!   is a `segmentby` column of every purged hypertable, so TimescaleDB drops
//!   whole compressed batches instead of decompressing them. An uncompressed
//!   chunk is purged in bounded `ctid` batches.
//! - **Plain tables** are purged in the same bounded `ctid` batches.
//! - **Pacing:** one purge at a time, a statement timeout and a pause between
//!   batches, so live ingest keeps priority on a small machine.
//! - **Resume and retry:** every statement is its own transaction and deletes
//!   by `project_id`, so a pass interrupted by a restart or an error simply
//!   runs again from the start and finds less to do. Failures are recorded on
//!   the row and retried with exponential backoff.
//! - **Completion:** a purge is complete only after a pass that started at
//!   least [`ProjectDataPurgeConfig::settle`] after the deletion removed
//!   nothing. That catches rows a batching writer flushed after the project
//!   was deleted.
//!
//! The rows are unreachable from the moment the project row is deleted: every
//! reader is scoped to a live project, and project ids come from a sequence,
//! so they are never reused. The purge only reclaims the space.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement, TransactionTrait,
};
use temps_core::{Job, JobQueue, JobReceiver};
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// Telemetry tables purged by `project_id` once their project is deleted.
///
/// Must match the tables whose foreign keys the
/// `m20261105_000001_project_data_purges` migration drops: a table with its
/// foreign key still in place is cleaned by the cascade, and a table missing
/// from here would keep a deleted project's rows forever.
pub const PURGED_TABLES: [&str; 5] = [
    "proxy_logs",
    "events",
    "error_events",
    "request_logs",
    "performance_metrics",
];

/// Longest `last_error` stored on a purge row.
const MAX_ERROR_LEN: usize = 2_000;

#[derive(Debug, Error)]
pub enum ProjectDataPurgeError {
    #[error("Failed to {operation} in the project data purge queue: {source}")]
    Queue {
        operation: &'static str,
        #[source]
        source: DbErr,
    },

    #[error("Failed to {step} while purging {relation} rows of deleted project {project_id}: {source}")]
    Purge {
        project_id: i32,
        relation: String,
        step: &'static str,
        #[source]
        source: DbErr,
    },
}

/// Pacing and timing of the purge worker.
#[derive(Debug, Clone)]
pub struct ProjectDataPurgeConfig {
    /// Rows removed per statement from an uncompressed relation.
    pub batch_size: i64,
    /// Pause after every statement, so ingest keeps priority.
    pub batch_pause: Duration,
    /// `statement_timeout` for every purge statement.
    pub statement_timeout: Duration,
    /// `timescaledb.max_tuples_decompressed_per_dml_transaction` for every
    /// purge statement. Purging by the `segmentby` column decompresses
    /// nothing, so this only bounds memory if TimescaleDB ever decides
    /// otherwise; the pass then fails with TimescaleDB's message and retries.
    pub max_decompressed_tuples: i64,
    /// How long a claimed purge stays reserved for its worker. Renewed after
    /// every statement, so it only has to outlast one.
    pub lease: Duration,
    /// How long after the deletion a pass must start for an empty pass to
    /// complete the purge.
    pub settle: Duration,
    /// How often the queue is checked without a `ProjectDeleted` job.
    pub poll_interval: Duration,
    /// Delay before the first retry of a failed pass; doubles per failure.
    pub retry_base: Duration,
    /// Longest delay between retries.
    pub retry_max: Duration,
}

impl Default for ProjectDataPurgeConfig {
    fn default() -> Self {
        Self {
            batch_size: 5_000,
            batch_pause: Duration::from_millis(25),
            statement_timeout: Duration::from_secs(60),
            max_decompressed_tuples: 100_000,
            lease: Duration::from_secs(600),
            settle: Duration::from_secs(300),
            poll_interval: Duration::from_secs(60),
            retry_base: Duration::from_secs(60),
            retry_max: Duration::from_secs(3_600),
        }
    }
}

/// What the database offers for purging hypertables, read once per pass.
#[derive(Debug, Clone, Copy, Default)]
struct TimescaleSupport {
    installed: bool,
    /// Whether `timescaledb.max_tuples_decompressed_per_dml_transaction`
    /// exists in this TimescaleDB version; setting it otherwise fails.
    decompression_limit: bool,
}

/// A purge reserved by this worker for one pass.
#[derive(Debug, Clone)]
struct ClaimedPurge {
    project_id: i32,
    project_name: String,
    requested_at: DateTime<Utc>,
    pass_started_at: DateTime<Utc>,
}

/// What one pass over a project's telemetry did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassOutcome {
    /// The queue had nothing due.
    Idle,
    /// A pass ran and removed this many rows; the purge stays pending.
    Removed(u64),
    /// The purge is complete.
    Completed,
    /// The pass failed; the error is recorded on the purge row.
    Failed,
    /// A project with this id exists again (e.g. restored from a backup), so
    /// nothing was deleted and the purge was closed.
    Cancelled,
}

pub struct ProjectDataPurger {
    db: Arc<DatabaseConnection>,
    config: ProjectDataPurgeConfig,
}

impl ProjectDataPurger {
    pub fn new(db: Arc<DatabaseConnection>, config: ProjectDataPurgeConfig) -> Self {
        Self { db, config }
    }

    /// Record that `project_id`'s telemetry must be purged. Call it on the
    /// transaction that deletes the project row, so the two commit together.
    /// A repeat for the same project is a no-op.
    pub async fn enqueue<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
        project_name: &str,
    ) -> Result<(), DbErr> {
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO project_data_purges (project_id, project_name) VALUES ($1, $2) \
             ON CONFLICT (project_id) DO NOTHING",
            [project_id.into(), project_name.into()],
        ))
        .await?;
        Ok(())
    }

    /// Run the worker: drain the queue, then wait for a `ProjectDeleted` job
    /// or the poll interval, forever.
    pub fn spawn(self: Arc<Self>, job_queue: Arc<dyn JobQueue>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut receiver = job_queue.subscribe();
            let mut interval = tokio::time::interval(self.config.poll_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                self.drain().await;
                Self::wait_for_work(receiver.as_mut(), &mut interval).await;
            }
        })
    }

    /// Wait until a project is deleted or the poll interval elapses.
    async fn wait_for_work(receiver: &mut dyn JobReceiver, interval: &mut tokio::time::Interval) {
        let mut receiver_failed = false;
        loop {
            if receiver_failed {
                interval.tick().await;
                return;
            }
            tokio::select! {
                _ = interval.tick() => return,
                job = receiver.recv() => match job {
                    Ok(Job::ProjectDeleted(job)) => {
                        debug!(project_id = job.project_id, "Project deleted; purging its telemetry");
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        // Lagging or a closed channel: the poll interval
                        // still finds every pending purge.
                        warn!(%error, "Project data purger missed job notifications; polling instead");
                        receiver_failed = true;
                    }
                },
            }
        }
    }

    /// Run passes until nothing is due.
    pub async fn drain(&self) {
        loop {
            match self.run_once().await {
                Ok(PassOutcome::Idle) => return,
                Ok(_) => {}
                Err(error) => {
                    error!(%error, "Project data purge queue is unavailable; retrying later");
                    return;
                }
            }
        }
    }

    /// Claim the next due purge and run one pass over it.
    pub async fn run_once(&self) -> Result<PassOutcome, ProjectDataPurgeError> {
        let Some(claim) = self.claim_next().await? else {
            return Ok(PassOutcome::Idle);
        };

        if self.project_exists(claim.project_id).await? {
            warn!(
                project_id = claim.project_id,
                project_name = %claim.project_name,
                "A project with this id exists again; closing its data purge without deleting anything"
            );
            self.complete(
                claim.project_id,
                Some("cancelled: a project with this id exists again"),
            )
            .await?;
            return Ok(PassOutcome::Cancelled);
        }

        match self.purge_pass(&claim).await {
            Ok(removed) => self.finish_pass(&claim, removed).await,
            Err(error) => {
                warn!(
                    project_id = claim.project_id,
                    project_name = %claim.project_name,
                    %error,
                    "Project data purge pass failed; it will be retried"
                );
                self.record_failure(claim.project_id, &error).await?;
                Ok(PassOutcome::Failed)
            }
        }
    }

    async fn claim_next(&self) -> Result<Option<ClaimedPurge>, ProjectDataPurgeError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE project_data_purges \
                 SET lease_until = now() + $1 * interval '1 second', passes = passes + 1 \
                 WHERE project_id = ( \
                     SELECT project_id FROM project_data_purges \
                     WHERE completed_at IS NULL AND next_attempt_at <= now() \
                       AND (lease_until IS NULL OR lease_until < now()) \
                     ORDER BY next_attempt_at, project_id \
                     LIMIT 1 FOR UPDATE SKIP LOCKED) \
                 RETURNING project_id, project_name, requested_at, now() AS pass_started_at",
                [(self.config.lease.as_secs() as i64).into()],
            ))
            .await
            .map_err(|source| ProjectDataPurgeError::Queue {
                operation: "claim the next due purge",
                source,
            })?;
        let Some(row) = row else {
            return Ok(None);
        };
        let decode = |source| ProjectDataPurgeError::Queue {
            operation: "decode a claimed purge",
            source,
        };
        Ok(Some(ClaimedPurge {
            project_id: row.try_get("", "project_id").map_err(decode)?,
            project_name: row.try_get("", "project_name").map_err(decode)?,
            requested_at: row.try_get("", "requested_at").map_err(decode)?,
            pass_started_at: row.try_get("", "pass_started_at").map_err(decode)?,
        }))
    }

    async fn project_exists(&self, project_id: i32) -> Result<bool, ProjectDataPurgeError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1) AS exists",
                [project_id.into()],
            ))
            .await
            .and_then(|row| match row {
                Some(row) => row.try_get::<bool>("", "exists"),
                None => Ok(false),
            })
            .map_err(|source| ProjectDataPurgeError::Queue {
                operation: "check whether the purged project exists",
                source,
            })?;
        Ok(row)
    }

    /// One pass over every purged table. Returns the rows removed.
    async fn purge_pass(&self, claim: &ClaimedPurge) -> Result<u64, ProjectDataPurgeError> {
        let timescale = self.timescale_support(claim.project_id).await?;
        let mut removed = 0;
        for table in PURGED_TABLES {
            removed += self.purge_table(claim.project_id, table, timescale).await?;
        }
        Ok(removed)
    }

    async fn timescale_support(
        &self,
        project_id: i32,
    ) -> Result<TimescaleSupport, ProjectDataPurgeError> {
        let row = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') \
                        AS installed, \
                        EXISTS (SELECT 1 FROM pg_settings WHERE name = \
                            'timescaledb.max_tuples_decompressed_per_dml_transaction') \
                        AS decompression_limit",
            ))
            .await
            .and_then(|row| match row {
                Some(row) => Ok(TimescaleSupport {
                    installed: row.try_get("", "installed")?,
                    decompression_limit: row.try_get("", "decompression_limit")?,
                }),
                None => Ok(TimescaleSupport::default()),
            })
            .map_err(|source| ProjectDataPurgeError::Purge {
                project_id,
                relation: "pg_extension".to_string(),
                step: "check for TimescaleDB",
                source,
            })?;
        Ok(row)
    }

    async fn purge_table(
        &self,
        project_id: i32,
        table: &str,
        timescale: TimescaleSupport,
    ) -> Result<u64, ProjectDataPurgeError> {
        let purge_error = |step: &'static str| {
            move |source| ProjectDataPurgeError::Purge {
                project_id,
                relation: table.to_string(),
                step,
                source,
            }
        };

        let exists = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT to_regclass($1) IS NOT NULL AS exists",
                [table.into()],
            ))
            .await
            .and_then(|row| match row {
                Some(row) => row.try_get::<bool>("", "exists"),
                None => Ok(false),
            })
            .map_err(purge_error("look up the table"))?;
        if !exists {
            return Ok(0);
        }

        let removed = if timescale.installed && self.is_hypertable(project_id, table).await? {
            // A ctid only identifies a row within one physical relation, so a
            // hypertable is purged chunk by chunk and never through its parent.
            let chunks = self
                .db
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT format('%I.%I', c.chunk_schema, c.chunk_name) AS chunk, \
                            c.is_compressed \
                     FROM timescaledb_information.chunks c \
                     WHERE format('%I.%I', c.hypertable_schema, c.hypertable_name)::regclass \
                           = to_regclass($1) \
                     ORDER BY c.range_start, c.chunk_name",
                    [table.into()],
                ))
                .await
                .map_err(purge_error("list chunks"))?
                .into_iter()
                .map(|row| {
                    Ok::<_, DbErr>((
                        row.try_get::<String>("", "chunk")?,
                        row.try_get::<bool>("", "is_compressed")?,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(purge_error("decode chunks"))?;
            let mut removed = 0;
            for (chunk, is_compressed) in chunks {
                removed += if is_compressed {
                    self.purge_compressed_chunk(project_id, &chunk, timescale)
                        .await?
                } else {
                    self.purge_relation_in_batches(project_id, &chunk, timescale)
                        .await?
                };
            }
            removed
        } else {
            self.purge_relation_in_batches(project_id, &format!("\"{table}\""), timescale)
                .await?
        };
        if removed > 0 {
            debug!(project_id, table, removed, "Purged deleted project's telemetry");
        }
        Ok(removed)
    }

    async fn is_hypertable(&self, project_id: i32, table: &str) -> Result<bool, ProjectDataPurgeError> {
        self.db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT EXISTS ( \
                     SELECT 1 FROM timescaledb_information.hypertables h \
                     WHERE format('%I.%I', h.hypertable_schema, h.hypertable_name)::regclass \
                           = to_regclass($1)) AS exists",
                [table.into()],
            ))
            .await
            .and_then(|row| match row {
                Some(row) => row.try_get::<bool>("", "exists"),
                None => Ok(false),
            })
            .map_err(|source| ProjectDataPurgeError::Purge {
                project_id,
                relation: table.to_string(),
                step: "check whether it is a hypertable",
                source,
            })
    }

    /// Remove the project's rows from a compressed chunk in one statement.
    /// The predicate is on the `segmentby` column alone, so whole compressed
    /// batches are dropped without decompression.
    async fn purge_compressed_chunk(
        &self,
        project_id: i32,
        chunk: &str,
        timescale: TimescaleSupport,
    ) -> Result<u64, ProjectDataPurgeError> {
        let sql = format!("DELETE FROM {chunk} WHERE project_id = $1");
        self.execute_batch(project_id, chunk, &sql, timescale).await
    }

    /// Remove the project's rows from a plain table or an uncompressed chunk,
    /// at most `batch_size` per statement.
    async fn purge_relation_in_batches(
        &self,
        project_id: i32,
        relation: &str,
        timescale: TimescaleSupport,
    ) -> Result<u64, ProjectDataPurgeError> {
        // `project_id` is checked again on the outer delete, so a row can
        // only ever be removed if it belongs to the purged project.
        let sql = format!(
            "DELETE FROM {relation} WHERE project_id = $1 AND ctid = ANY(ARRAY( \
                 SELECT ctid FROM {relation} WHERE project_id = $1 LIMIT {limit}))",
            limit = self.config.batch_size,
        );
        let mut removed = 0;
        loop {
            let batch = self
                .execute_batch(project_id, relation, &sql, timescale)
                .await?;
            removed += batch;
            if batch < self.config.batch_size as u64 {
                return Ok(removed);
            }
        }
    }

    /// Run one purge statement in its own transaction under the statement
    /// timeout, then renew the lease, record progress and pause.
    async fn execute_batch(
        &self,
        project_id: i32,
        relation: &str,
        sql: &str,
        timescale: TimescaleSupport,
    ) -> Result<u64, ProjectDataPurgeError> {
        let purge_error = |step: &'static str| {
            move |source| ProjectDataPurgeError::Purge {
                project_id,
                relation: relation.to_string(),
                step,
                source,
            }
        };
        let txn = self.db.begin().await.map_err(purge_error("begin a transaction"))?;
        txn.execute_unprepared(&format!(
            "SET LOCAL statement_timeout = {}",
            self.config.statement_timeout.as_millis()
        ))
        .await
        .map_err(purge_error("set the statement timeout"))?;
        if timescale.decompression_limit {
            txn.execute_unprepared(&format!(
                "SET LOCAL timescaledb.max_tuples_decompressed_per_dml_transaction = {}",
                self.config.max_decompressed_tuples
            ))
            .await
            .map_err(purge_error("set the decompression limit"))?;
        }
        let removed = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [project_id.into()],
            ))
            .await
            .map_err(purge_error("delete rows"))?
            .rows_affected();
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE project_data_purges \
             SET lease_until = now() + $2 * interval '1 second', rows_deleted = rows_deleted + $3 \
             WHERE project_id = $1",
            [
                project_id.into(),
                (self.config.lease.as_secs() as i64).into(),
                (removed as i64).into(),
            ],
        ))
        .await
        .map_err(purge_error("record progress"))?;
        txn.commit().await.map_err(purge_error("commit"))?;
        if !self.config.batch_pause.is_zero() {
            tokio::time::sleep(self.config.batch_pause).await;
        }
        Ok(removed)
    }

    async fn finish_pass(
        &self,
        claim: &ClaimedPurge,
        removed: u64,
    ) -> Result<PassOutcome, ProjectDataPurgeError> {
        // A settle window too large for chrono never elapses.
        let settled = match chrono::Duration::from_std(self.config.settle) {
            Ok(settle) => claim.pass_started_at >= claim.requested_at + settle,
            Err(_) => false,
        };
        if removed == 0 && settled {
            self.complete(claim.project_id, None).await?;
            info!(
                project_id = claim.project_id,
                project_name = %claim.project_name,
                "Deleted project's telemetry is purged"
            );
            return Ok(PassOutcome::Completed);
        }

        // Rows found: verify again once the settle window has passed, which
        // also catches rows a batching writer flushed after the delete. No
        // rows yet but too early: wait out the rest of the window.
        let next_attempt_sql = if removed > 0 {
            "now() + $2 * interval '1 second'"
        } else {
            "GREATEST(now(), requested_at + $2 * interval '1 second')"
        };
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE project_data_purges \
                     SET next_attempt_at = {next_attempt_sql}, lease_until = NULL, \
                         consecutive_failures = 0, last_error = NULL \
                     WHERE project_id = $1"
                ),
                [
                    claim.project_id.into(),
                    (self.config.settle.as_secs() as i64).into(),
                ],
            ))
            .await
            .map_err(|source| ProjectDataPurgeError::Queue {
                operation: "schedule the next purge pass",
                source,
            })?;
        if removed > 0 {
            info!(
                project_id = claim.project_id,
                project_name = %claim.project_name,
                removed,
                "Purged deleted project's telemetry; verifying again after the settle window"
            );
        }
        Ok(PassOutcome::Removed(removed))
    }

    async fn complete(&self, project_id: i32, note: Option<&str>) -> Result<(), ProjectDataPurgeError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE project_data_purges \
                 SET completed_at = now(), lease_until = NULL, consecutive_failures = 0, \
                     last_error = $2 \
                 WHERE project_id = $1",
                [project_id.into(), note.map(str::to_string).into()],
            ))
            .await
            .map_err(|source| ProjectDataPurgeError::Queue {
                operation: "mark a purge complete",
                source,
            })?;
        Ok(())
    }

    async fn record_failure(
        &self,
        project_id: i32,
        error: &ProjectDataPurgeError,
    ) -> Result<(), ProjectDataPurgeError> {
        let mut message = error.to_string();
        if message.len() > MAX_ERROR_LEN {
            let mut cut = MAX_ERROR_LEN;
            while !message.is_char_boundary(cut) {
                cut -= 1;
            }
            message.truncate(cut);
        }
        // Backoff doubles per consecutive failure: base, 2×base, ... capped.
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE project_data_purges \
                 SET consecutive_failures = consecutive_failures + 1, \
                     last_error = $2, \
                     lease_until = NULL, \
                     next_attempt_at = now() + LEAST($4::float8, \
                         $3::float8 * power(2, LEAST(consecutive_failures, 30))) \
                         * interval '1 second' \
                 WHERE project_id = $1",
                [
                    project_id.into(),
                    message.into(),
                    self.config.retry_base.as_secs_f64().into(),
                    self.config.retry_max.as_secs_f64().into(),
                ],
            ))
            .await
            .map_err(|source| ProjectDataPurgeError::Queue {
                operation: "record a failed purge pass",
                source,
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Statement};
    use temps_database::test_utils::TestDatabase;

    fn fast_config() -> ProjectDataPurgeConfig {
        ProjectDataPurgeConfig {
            batch_size: 2,
            batch_pause: Duration::ZERO,
            settle: Duration::ZERO,
            retry_base: Duration::from_secs(60),
            ..ProjectDataPurgeConfig::default()
        }
    }

    #[test]
    fn purged_tables_match_the_migration() {
        assert_eq!(
            PURGED_TABLES,
            temps_migrations::PROJECT_DATA_PURGED_TABLES,
            "the purger must cover exactly the tables the migration detached from projects"
        );
    }

    async fn exec(db: &DatabaseConnection, sql: &str) {
        db.execute_unprepared(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
        db.query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .and_then(|row| row.try_get::<i64>("", "n").ok())
            .unwrap_or_else(|| panic!("{sql}: no count"))
    }

    /// Seed telemetry for two project ids directly; neither has a project row,
    /// which is exactly the state after a delete now that the foreign keys are
    /// gone.
    async fn seed_telemetry(db: &DatabaseConnection, project_id: i32, rows: i32) {
        insert_proxy_logs(db, project_id, rows, "now()").await;
        exec(
            db,
            &format!(
                "INSERT INTO request_logs (project_id, environment_id, deployment_id, date, host, \
                     method, request_path, message, status_code, request_id, level, user_agent, \
                     started_at, finished_at) \
                 SELECT {project_id}, 1, 1, '2026-10-30', 'example.test', 'GET', '/', '', 200, \
                     'req-' || g, 'info', 'test', '', '' \
                 FROM generate_series(1, {rows}) g"
            ),
        )
        .await;
    }

    /// `rows` proxy log lines for `project_id`, one minute apart, ending at `until`.
    async fn insert_proxy_logs(db: &DatabaseConnection, project_id: i32, rows: i32, until: &str) {
        exec(
            db,
            &format!(
                "INSERT INTO proxy_logs (timestamp, project_id, method, path, host, status_code, \
                     request_source, routing_status, request_id, created_date) \
                 SELECT {until} - g * interval '1 minute', {project_id}, 'GET', '/p', \
                     'example.test', 200, 'proxy', 'routed', 'req-' || g, \
                     ({until} - g * interval '1 minute')::date \
                 FROM generate_series(1, {rows}) g"
            ),
        )
        .await;
    }

    async fn test_db() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(e) => {
                println!("Docker/Postgres not available, skipping: {e}");
                None
            }
        }
    }

    #[tokio::test]
    async fn purges_only_the_deleted_projects_rows_and_completes() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.clone();
        seed_telemetry(&db, 9_001, 5).await;
        seed_telemetry(&db, 9_002, 3).await;
        ProjectDataPurger::enqueue(db.as_ref(), 9_001, "deleted-app")
            .await
            .unwrap();

        let purger = ProjectDataPurger::new(db.clone(), fast_config());
        assert_eq!(purger.run_once().await.unwrap(), PassOutcome::Removed(10));
        // Nothing left and the settle window (zero here) has passed.
        assert_eq!(purger.run_once().await.unwrap(), PassOutcome::Completed);
        assert_eq!(purger.run_once().await.unwrap(), PassOutcome::Idle);

        for table in ["proxy_logs", "request_logs"] {
            assert_eq!(
                count(&db, &format!("SELECT count(*) AS n FROM {table} WHERE project_id = 9001")).await,
                0,
                "{table} still holds the deleted project's rows"
            );
            assert!(
                count(&db, &format!("SELECT count(*) AS n FROM {table} WHERE project_id = 9002")).await > 0,
                "{table} lost another project's rows"
            );
        }
        assert_eq!(
            count(
                &db,
                "SELECT rows_deleted AS n FROM project_data_purges WHERE project_id = 9001 \
                 AND completed_at IS NOT NULL"
            )
            .await,
            10
        );
    }

    #[tokio::test]
    async fn purges_compressed_chunks() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.clone();
        for project_id in [9_101, 9_102] {
            insert_proxy_logs(&db, project_id, 50, "(now() - interval '3 days')").await;
        }
        exec(
            &db,
            "SELECT compress_chunk(c, if_not_compressed => true) \
             FROM show_chunks('proxy_logs', older_than => interval '1 day') c",
        )
        .await;
        assert!(
            count(
                &db,
                "SELECT count(*) AS n FROM timescaledb_information.chunks \
                 WHERE hypertable_name = 'proxy_logs' AND is_compressed \
                   AND hypertable_schema = current_schema()"
            )
            .await
                > 0,
            "the test needs a compressed chunk"
        );
        ProjectDataPurger::enqueue(db.as_ref(), 9_101, "compressed-app")
            .await
            .unwrap();
        // A decompression ceiling of one tuple: purging by the segmentby
        // column must drop whole batches and decompress nothing.
        let purger = ProjectDataPurger::new(
            db.clone(),
            ProjectDataPurgeConfig {
                max_decompressed_tuples: 1,
                ..fast_config()
            },
        );
        let outcome = purger.run_once().await.unwrap();
        assert_eq!(outcome, PassOutcome::Removed(50));
        assert_eq!(
            count(&db, "SELECT count(*) AS n FROM proxy_logs WHERE project_id = 9101").await,
            0
        );
        assert_eq!(
            count(&db, "SELECT count(*) AS n FROM proxy_logs WHERE project_id = 9102").await,
            50
        );
    }

    #[tokio::test]
    async fn waits_for_the_settle_window_before_completing() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.clone();
        ProjectDataPurger::enqueue(db.as_ref(), 9_201, "late-writes")
            .await
            .unwrap();
        let purger = ProjectDataPurger::new(
            db.clone(),
            ProjectDataPurgeConfig {
                settle: Duration::from_secs(300),
                ..fast_config()
            },
        );
        assert_eq!(purger.run_once().await.unwrap(), PassOutcome::Removed(0));
        // Rescheduled to the end of the settle window, so not due yet.
        assert_eq!(purger.run_once().await.unwrap(), PassOutcome::Idle);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) AS n FROM project_data_purges WHERE project_id = 9201 \
                 AND completed_at IS NULL AND next_attempt_at > now() + interval '4 minutes'"
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn records_a_failed_pass_and_backs_off() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.clone();
        seed_telemetry(&db, 9_301, 2).await;
        ProjectDataPurger::enqueue(db.as_ref(), 9_301, "failing-app")
            .await
            .unwrap();
        // Hold a lock the purge needs; its statement timeout then fails the pass.
        let blocker = db.begin().await.unwrap();
        blocker
            .execute_unprepared("LOCK TABLE request_logs IN ACCESS EXCLUSIVE MODE")
            .await
            .unwrap();

        let purger = ProjectDataPurger::new(
            db.clone(),
            ProjectDataPurgeConfig {
                statement_timeout: Duration::from_millis(200),
                ..fast_config()
            },
        );
        let outcome = purger.run_once().await.unwrap();
        blocker.rollback().await.unwrap();

        assert_eq!(outcome, PassOutcome::Failed);
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT consecutive_failures, last_error, completed_at IS NULL AS pending, \
                     next_attempt_at > now() + interval '30 seconds' AS backed_off \
                 FROM project_data_purges WHERE project_id = 9301",
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<i32>("", "consecutive_failures").unwrap(), 1);
        let last_error: String = row.try_get("", "last_error").unwrap();
        assert!(
            last_error.contains("deleted project 9301") && last_error.contains("request_logs"),
            "last_error must name the project and the relation: {last_error}"
        );
        assert!(row.try_get::<bool>("", "pending").unwrap());
        assert!(row.try_get::<bool>("", "backed_off").unwrap());
        // Not due again until the backoff elapses.
        assert_eq!(purger.run_once().await.unwrap(), PassOutcome::Idle);
    }

    #[tokio::test]
    async fn never_purges_a_project_that_exists() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.clone();
        exec(
            &db,
            "INSERT INTO projects (id, name, slug, repo_name, repo_owner, directory, main_branch, \
                 preset, created_at, updated_at) \
             VALUES (9401, 'alive', 'alive', 'r', 'o', '.', 'main', 'static', now(), now())",
        )
        .await;
        seed_telemetry(&db, 9_401, 2).await;
        ProjectDataPurger::enqueue(db.as_ref(), 9_401, "alive")
            .await
            .unwrap();

        let purger = ProjectDataPurger::new(db.clone(), fast_config());
        assert_eq!(purger.run_once().await.unwrap(), PassOutcome::Cancelled);
        assert_eq!(
            count(&db, "SELECT count(*) AS n FROM proxy_logs WHERE project_id = 9401").await,
            2
        );
    }
}
