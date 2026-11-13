// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`WorkflowStore`] on the control-plane Postgres.
//!
//! The claim is one transaction under a transaction-scoped advisory lock, so
//! concurrent control-plane processes never count the same free concurrency
//! slot twice; rows another transaction holds (an event delivery, a cancel)
//! are skipped with `SKIP LOCKED` rather than waited for. Every scan is bounded
//! (`LIMIT`) and served by a partial index of the rows it can match.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
    TransactionTrait, Value,
};
use temps_entities::{
    environments, workflow_credentials, workflow_environment_settings, workflow_events,
    workflow_runs, workflow_steps,
};
use uuid::Uuid;

use super::*;
use crate::error::{WorkflowError, WorkflowResult};
use crate::protocol::CHILD_EVENT;

/// Key of the transaction-scoped advisory lock that serializes claims.
const CLAIM_LOCK_KEY: i64 = 7_343_019_001;
/// Class of the per-environment advisory lock (`pg_advisory_xact_lock(class,
/// environment_id)`) that serializes starts, so concurrent starts cannot both
/// pass the `max_active_runs` count.
const ADMISSION_LOCK_CLASS: i32 = 734_302;
/// Most due rows one claim ranks. Rows beyond it wait for a later claim; the
/// window keeps a claim's cost bounded however large the backlog grows.
const CLAIM_WINDOW: i64 = 10_000;
/// The NOTIFY channel the scheduler listens on.
pub const DUE_CHANNEL: &str = "workflow_runs_due";

const RUNNABLE: &str = "('queued', 'sleeping', 'waiting', 'retrying')";
const TERMINAL: &str = "('completed', 'failed', 'cancelled')";

pub struct PgWorkflowStore {
    db: Arc<DatabaseConnection>,
}

impl PgWorkflowStore {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn status_of(row: &RunRow) -> RunStatus {
    RunStatus::parse(&row.status).unwrap_or(RunStatus::Failed)
}

fn stale(row: Option<&RunRow>, run_id: Uuid, presented: i64) -> WorkflowError {
    match row {
        Some(row) => WorkflowError::StaleLease {
            run_id,
            presented,
            current: row.lease_token,
            status: row.status.clone(),
        },
        None => WorkflowError::StaleLease {
            run_id,
            presented,
            current: -1,
            status: "deleted".into(),
        },
    }
}

async fn notify<C: ConnectionTrait>(conn: &C) -> WorkflowResult<()> {
    conn.execute(stmt("SELECT pg_notify($1, '')", vec![DUE_CHANNEL.into()]))
        .await
        .map_err(WorkflowError::db("notifying the workflow scheduler"))?;
    Ok(())
}

#[derive(Debug, FromQueryResult)]
struct WaitingRow {
    id: Uuid,
    project_id: i32,
    environment_id: i32,
    lease_token: i64,
    awaited_sequence: Option<i32>,
    awaited_event_name: Option<String>,
    awaited_event_key: Option<String>,
}

impl WaitingRow {
    fn into_waiting(self) -> Option<WaitingRun> {
        Some(WaitingRun {
            run_id: self.id,
            project_id: self.project_id,
            environment_id: self.environment_id,
            lease: self.lease_token,
            sequence: self.awaited_sequence?,
            event_name: self.awaited_event_name?,
            event_key: self.awaited_event_key.unwrap_or_default(),
        })
    }
}

#[async_trait]
impl WorkflowStore for PgWorkflowStore {
    async fn environment_project(&self, environment_id: i32) -> WorkflowResult<Option<i32>> {
        Ok(environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("loading an environment"))?
            .filter(|environment| environment.deleted_at.is_none())
            .map(|environment| environment.project_id))
    }

    async fn settings(&self, environment_id: i32) -> WorkflowResult<WorkflowSettings> {
        Ok(
            workflow_environment_settings::Entity::find_by_id(environment_id)
                .one(self.db.as_ref())
                .await
                .map_err(WorkflowError::db("loading workflow settings"))?
                .map(|row| WorkflowSettings::from(&row))
                .unwrap_or_default(),
        )
    }

    async fn upsert_settings(
        &self,
        project_id: i32,
        environment_id: i32,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<WorkflowSettings> {
        let row = workflow_environment_settings::Entity::find()
            .from_raw_sql(stmt(
                "INSERT INTO workflow_environment_settings (environment_id, project_id, host_service, \
                 invocation_deadline_secs, lease_secs, max_concurrent_runs, max_active_runs, \
                 max_steps_per_run, max_journal_bytes_per_run, inline_payload_limit_bytes, \
                 max_payload_bytes, retention_days, stuck_after_failures) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
                 ON CONFLICT (environment_id) DO UPDATE SET host_service = EXCLUDED.host_service, \
                 invocation_deadline_secs = EXCLUDED.invocation_deadline_secs, \
                 lease_secs = EXCLUDED.lease_secs, max_concurrent_runs = EXCLUDED.max_concurrent_runs, \
                 max_active_runs = EXCLUDED.max_active_runs, max_steps_per_run = EXCLUDED.max_steps_per_run, \
                 max_journal_bytes_per_run = EXCLUDED.max_journal_bytes_per_run, \
                 inline_payload_limit_bytes = EXCLUDED.inline_payload_limit_bytes, \
                 max_payload_bytes = EXCLUDED.max_payload_bytes, retention_days = EXCLUDED.retention_days, \
                 stuck_after_failures = EXCLUDED.stuck_after_failures, updated_at = now() \
                 RETURNING *",
                vec![
                    environment_id.into(),
                    project_id.into(),
                    settings.host_service.clone().into(),
                    settings.invocation_deadline_secs.into(),
                    settings.lease_secs.into(),
                    settings.max_concurrent_runs.into(),
                    settings.max_active_runs.into(),
                    settings.max_steps_per_run.into(),
                    settings.max_journal_bytes_per_run.into(),
                    settings.inline_payload_limit_bytes.into(),
                    settings.max_payload_bytes.into(),
                    settings.retention_days.into(),
                    settings.stuck_after_failures.into(),
                ],
            ))
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("saving workflow settings"))?
            .ok_or(WorkflowError::Database {
                operation: "saving workflow settings",
                source: sea_orm::DbErr::RecordNotInserted,
            })?;
        Ok(WorkflowSettings::from(&row))
    }

    async fn active_credential(
        &self,
        environment_id: i32,
    ) -> WorkflowResult<Option<CredentialRow>> {
        workflow_credentials::Entity::find()
            .filter(workflow_credentials::Column::EnvironmentId.eq(environment_id))
            .filter(workflow_credentials::Column::RevokedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("loading the workflow credential"))
    }

    async fn rotate_credential(
        &self,
        project_id: i32,
        environment_id: i32,
        hash: &str,
        prefix: &str,
        user_id: Option<i32>,
    ) -> WorkflowResult<CredentialRow> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("rotating the workflow credential"))?;
        txn.execute(stmt(
            "UPDATE workflow_credentials SET revoked_at = now() \
             WHERE environment_id = $1 AND revoked_at IS NULL",
            vec![environment_id.into()],
        ))
        .await
        .map_err(WorkflowError::db(
            "revoking the previous workflow credential",
        ))?;
        let row = workflow_credentials::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            token_hash: Set(hash.to_string()),
            token_prefix: Set(prefix.to_string()),
            created_by_user_id: Set(user_id),
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(WorkflowError::db("storing the workflow credential"))?;
        txn.commit()
            .await
            .map_err(WorkflowError::db("rotating the workflow credential"))?;
        Ok(row)
    }

    async fn revoke_credential(&self, environment_id: i32) -> WorkflowResult<bool> {
        let result = self
            .db
            .execute(stmt(
                "UPDATE workflow_credentials SET revoked_at = now() \
                 WHERE environment_id = $1 AND revoked_at IS NULL",
                vec![environment_id.into()],
            ))
            .await
            .map_err(WorkflowError::db("revoking the workflow credential"))?;
        Ok(result.rows_affected() > 0)
    }

    async fn credential_by_hash(&self, hash: &str) -> WorkflowResult<Option<CredentialRow>> {
        let touched = workflow_credentials::Entity::find()
            .from_raw_sql(stmt(
                "UPDATE workflow_credentials SET last_used_at = now() \
                 WHERE token_hash = $1 AND revoked_at IS NULL \
                 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute') \
                 RETURNING *",
                vec![hash.into()],
            ))
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("authenticating a workflow credential"))?;
        if touched.is_some() {
            return Ok(touched);
        }
        workflow_credentials::Entity::find()
            .filter(workflow_credentials::Column::TokenHash.eq(hash))
            .filter(workflow_credentials::Column::RevokedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("authenticating a workflow credential"))
    }

    async fn insert_run(&self, run: NewRun, max_active_runs: i64) -> WorkflowResult<StartOutcome> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("starting a workflow run"))?;
        txn.execute(stmt(
            "SELECT pg_advisory_xact_lock($1, $2)",
            vec![ADMISSION_LOCK_CLASS.into(), run.environment_id.into()],
        ))
        .await
        .map_err(WorkflowError::db("serializing workflow starts"))?;
        if let Some(key) = &run.idempotency_key {
            let existing = workflow_runs::Entity::find()
                .filter(workflow_runs::Column::EnvironmentId.eq(run.environment_id))
                .filter(workflow_runs::Column::WorkflowName.eq(run.workflow_name.clone()))
                .filter(workflow_runs::Column::IdempotencyKey.eq(key.clone()))
                .one(&txn)
                .await
                .map_err(WorkflowError::db("looking up a run by idempotency key"))?;
            if let Some(existing) = existing {
                return Ok(StartOutcome::Existing(existing));
            }
        }
        let active = txn
            .query_one(stmt(
                &format!(
                    "SELECT count(*)::BIGINT AS n FROM workflow_runs \
                     WHERE environment_id = $1 AND status NOT IN {TERMINAL}"
                ),
                vec![run.environment_id.into()],
            ))
            .await
            .map_err(WorkflowError::db("counting active workflow runs"))?
            .map(|row| row.try_get::<i64>("", "n"))
            .transpose()
            .map_err(WorkflowError::db("counting active workflow runs"))?
            .unwrap_or(0);
        if active >= max_active_runs {
            return Err(WorkflowError::QuotaExceeded {
                environment_id: run.environment_id,
                quota: "max_active_runs",
                limit: max_active_runs,
            });
        }
        let (user, credential) = match run.starter {
            Starter::User(id) => (Some(id), None),
            Starter::Credential(id) => (None, Some(id)),
            Starter::Machine => (None, None),
        };
        let inserted = workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                "INSERT INTO workflow_runs (id, project_id, environment_id, workflow_name, status, \
                 input_encrypted, input_blob_key, input_bytes, idempotency_key, deployment_id, \
                 started_by_user_id, started_by_credential_id, has_blob_payloads, parent_run_id, \
                 due_at) \
                 VALUES ($1, $2, $3, $4, 'queued', $5, $6, $7, $8, $9, $10, $11, $12, $13, now()) \
                 ON CONFLICT (environment_id, workflow_name, idempotency_key) \
                 WHERE idempotency_key IS NOT NULL DO NOTHING \
                 RETURNING *",
                vec![
                    run.id.into(),
                    run.project_id.into(),
                    run.environment_id.into(),
                    run.workflow_name.clone().into(),
                    run.input.encrypted.clone().into(),
                    run.input.blob_key.clone().into(),
                    run.input.bytes.into(),
                    run.idempotency_key.clone().into(),
                    run.deployment_id.into(),
                    user.into(),
                    credential.into(),
                    run.input.blob_key.is_some().into(),
                    run.parent_run_id.into(),
                ],
            ))
            .one(&txn)
            .await
            .map_err(WorkflowError::db("inserting a workflow run"))?;
        let outcome = match inserted {
            Some(row) => {
                notify(&txn).await?;
                StartOutcome::Created(row)
            }
            None => {
                // Another start with the same key won the race.
                let existing = workflow_runs::Entity::find()
                    .filter(workflow_runs::Column::EnvironmentId.eq(run.environment_id))
                    .filter(workflow_runs::Column::WorkflowName.eq(run.workflow_name.clone()))
                    .filter(workflow_runs::Column::IdempotencyKey.eq(run.idempotency_key.clone()))
                    .one(&txn)
                    .await
                    .map_err(WorkflowError::db("looking up a run by idempotency key"))?
                    .ok_or(WorkflowError::Validation {
                        field: "idempotency_key",
                        reason: "a concurrent start with this key did not leave a run".into(),
                    })?;
                StartOutcome::Existing(existing)
            }
        };
        txn.commit()
            .await
            .map_err(WorkflowError::db("starting a workflow run"))?;
        Ok(outcome)
    }

    async fn claim_due(
        &self,
        batch: u64,
        limits: &[WorkflowLimit],
    ) -> WorkflowResult<Vec<ClaimedRun>> {
        if batch == 0 {
            return Ok(Vec::new());
        }
        let limits_json = serde_json::Value::Array(
            limits
                .iter()
                .map(|limit| {
                    serde_json::json!({
                        "e": limit.environment_id,
                        "w": limit.workflow_name,
                        "lim": limit.limit,
                    })
                })
                .collect(),
        );
        let txn = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("claiming due workflow runs"))?;
        txn.execute(stmt(
            "SELECT pg_advisory_xact_lock($1)",
            vec![CLAIM_LOCK_KEY.into()],
        ))
        .await
        .map_err(WorkflowError::db("taking the workflow claim lock"))?;
        let sql = format!(
            "WITH running_env AS ( \
                SELECT environment_id, count(*) AS n FROM workflow_runs \
                WHERE status = 'running' GROUP BY environment_id \
             ), running_wf AS ( \
                SELECT environment_id, workflow_name, count(*) AS n FROM workflow_runs \
                WHERE status = 'running' GROUP BY environment_id, workflow_name \
             ), limits AS ( \
                SELECT e, w, lim FROM jsonb_to_recordset($2::jsonb) AS f(e int, w text, lim int) \
             ), due AS ( \
                SELECT id, environment_id, workflow_name, due_at FROM workflow_runs \
                WHERE status IN {RUNNABLE} AND due_at <= now() \
                ORDER BY due_at, id LIMIT {CLAIM_WINDOW} \
             ), ranked_wf AS ( \
                SELECT d.id, d.environment_id, d.due_at, \
                       row_number() OVER (PARTITION BY d.environment_id, d.workflow_name ORDER BY d.due_at, d.id) AS rn, \
                       COALESCE(l.lim - COALESCE(rw.n, 0), 2147483647) AS room \
                FROM due d \
                LEFT JOIN running_wf rw ON rw.environment_id = d.environment_id AND rw.workflow_name = d.workflow_name \
                LEFT JOIN limits l ON l.e = d.environment_id AND l.w = d.workflow_name \
             ), ranked_env AS ( \
                SELECT w.id, w.due_at, \
                       row_number() OVER (PARTITION BY w.environment_id ORDER BY w.due_at, w.id) AS rn, \
                       COALESCE(s.max_concurrent_runs, 50) - COALESCE(re.n, 0) AS room \
                FROM ranked_wf w \
                LEFT JOIN workflow_environment_settings s ON s.environment_id = w.environment_id \
                LEFT JOIN running_env re ON re.environment_id = w.environment_id \
                WHERE w.rn <= w.room \
             ), picked AS ( \
                SELECT id FROM ranked_env WHERE rn <= room ORDER BY due_at, id LIMIT $1 \
             ), candidates AS ( \
                SELECT r.id, r.status AS previous_status FROM workflow_runs r \
                WHERE r.id IN (SELECT id FROM picked) AND r.status IN {RUNNABLE} \
                FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE workflow_runs w SET status = 'running', attempt = w.attempt + 1, \
                lease_token = w.lease_token + 1, \
                lease_expires_at = now() + COALESCE((SELECT s.lease_secs FROM workflow_environment_settings s \
                    WHERE s.environment_id = w.environment_id), 120) * interval '1 second', \
                started_at = COALESCE(w.started_at, now()), blocked_reason = NULL, updated_at = now() \
             FROM candidates c WHERE w.id = c.id \
             RETURNING w.*, c.previous_status"
        );
        let rows = txn
            .query_all(stmt(
                &sql,
                vec![
                    i64::try_from(batch).unwrap_or(i64::MAX).into(),
                    limits_json.into(),
                ],
            ))
            .await
            .map_err(WorkflowError::db("claiming due workflow runs"))?;
        txn.commit()
            .await
            .map_err(WorkflowError::db("claiming due workflow runs"))?;
        rows.into_iter()
            .map(|row| {
                let run = RunRow::from_query_result(&row, "")
                    .map_err(WorkflowError::db("reading a claimed workflow run"))?;
                let previous: String = row
                    .try_get("", "previous_status")
                    .map_err(WorkflowError::db("reading a claimed workflow run"))?;
                Ok(ClaimedRun {
                    run,
                    previous_status: RunStatus::parse(&previous).unwrap_or(RunStatus::Queued),
                })
            })
            .collect()
    }

    async fn running_count(
        &self,
        environment_id: i32,
        workflow_name: &str,
        except: Uuid,
    ) -> WorkflowResult<i64> {
        let row = self
            .db
            .query_one(stmt(
                "SELECT count(*)::BIGINT AS n FROM workflow_runs \
                 WHERE environment_id = $1 AND workflow_name = $2 AND status = 'running' \
                 AND id <> $3",
                vec![environment_id.into(), workflow_name.into(), except.into()],
            ))
            .await
            .map_err(WorkflowError::db("counting running workflow runs"))?;
        row.map(|row| row.try_get::<i64>("", "n"))
            .transpose()
            .map_err(WorkflowError::db("counting running workflow runs"))
            .map(|count| count.unwrap_or(0))
    }

    async fn block_claimed(
        &self,
        run_id: Uuid,
        lease: i64,
        previous_status: RunStatus,
        reason: &str,
        retry_at: DateTime<Utc>,
    ) -> WorkflowResult<bool> {
        let result = self
            .db
            .execute(stmt(
                "UPDATE workflow_runs SET status = $3, attempt = GREATEST(attempt - 1, 0), \
                 lease_expires_at = NULL, due_at = $5, blocked_reason = $4, updated_at = now() \
                 WHERE id = $1 AND lease_token = $2 AND status = 'running'",
                vec![
                    run_id.into(),
                    lease.into(),
                    previous_status.as_str().into(),
                    reason.into(),
                    retry_at.into(),
                ],
            ))
            .await
            .map_err(WorkflowError::db("returning a blocked workflow run"))?;
        Ok(result.rows_affected() > 0)
    }

    async fn fail_invocation(
        &self,
        run_id: Uuid,
        lease: i64,
        reason: &str,
    ) -> WorkflowResult<Option<RunStatus>> {
        let row = self
            .db
            .query_one(stmt(
                "WITH t AS ( \
                    SELECT r.id, r.consecutive_failures + 1 AS failures, \
                           COALESCE(s.stuck_after_failures, 8) AS stuck_after \
                    FROM workflow_runs r \
                    LEFT JOIN workflow_environment_settings s ON s.environment_id = r.environment_id \
                    WHERE r.id = $1 AND r.lease_token = $2 AND r.status = 'running' \
                    FOR UPDATE OF r \
                 ) \
                 UPDATE workflow_runs r SET consecutive_failures = t.failures, \
                    status = CASE WHEN t.failures >= t.stuck_after THEN 'stuck' \
                                  WHEN r.awaited_event_name IS NOT NULL THEN 'waiting' \
                                  ELSE 'retrying' END, \
                    due_at = CASE WHEN t.failures >= t.stuck_after THEN NULL \
                                  ELSE now() + LEAST(300, 5 * (2 ^ LEAST(t.failures - 1, 6)))::int * interval '1 second' END, \
                    lease_expires_at = NULL, blocked_reason = $3, updated_at = now() \
                 FROM t WHERE r.id = t.id RETURNING r.status",
                vec![run_id.into(), lease.into(), reason.into()],
            ))
            .await
            .map_err(WorkflowError::db("recording a failed invocation"))?;
        row.map(|row| {
            row.try_get::<String>("", "status")
                .map(|status| RunStatus::parse(&status).unwrap_or(RunStatus::Retrying))
                .map_err(WorkflowError::db("recording a failed invocation"))
        })
        .transpose()
    }

    async fn heartbeat(&self, run_id: Uuid, lease: i64) -> WorkflowResult<DateTime<Utc>> {
        let row = self
            .db
            .query_one(stmt(
                "UPDATE workflow_runs r SET lease_expires_at = now() + COALESCE( \
                    (SELECT s.lease_secs FROM workflow_environment_settings s \
                     WHERE s.environment_id = r.environment_id), 120) * interval '1 second', \
                 updated_at = now() \
                 WHERE r.id = $1 AND r.lease_token = $2 AND r.status = 'running' \
                 RETURNING r.lease_expires_at",
                vec![run_id.into(), lease.into()],
            ))
            .await
            .map_err(WorkflowError::db("extending a workflow lease"))?;
        match row {
            Some(row) => row
                .try_get::<DateTime<Utc>>("", "lease_expires_at")
                .map_err(WorkflowError::db("extending a workflow lease")),
            None => {
                let current = self.get_run(run_id).await?;
                Err(stale(current.as_ref(), run_id, lease))
            }
        }
    }

    async fn release(&self, run_id: Uuid, lease: i64, release: Release) -> WorkflowResult<RunRow> {
        let fence = "WHERE id = $1 AND lease_token = $2 AND status = 'running' RETURNING *";
        let (sql, extra, wake_now): (String, Vec<Value>, bool) = match release {
            Release::Completed { output } => (
                format!(
                    "UPDATE workflow_runs SET status = 'completed', output_encrypted = $3, \
                     output_blob_key = $4, output_bytes = $5, \
                     has_blob_payloads = has_blob_payloads OR $4 IS NOT NULL, \
                     finished_at = now(), due_at = NULL, lease_expires_at = NULL, \
                     awaited_event_name = NULL, awaited_event_key = NULL, awaited_sequence = NULL, \
                     consecutive_failures = 0, blocked_reason = NULL, updated_at = now() {fence}"
                ),
                vec![
                    output.encrypted.into(),
                    output.blob_key.into(),
                    output.bytes.into(),
                ],
                false,
            ),
            Release::Failed { error, kind } => (
                format!(
                    "UPDATE workflow_runs SET status = 'failed', error_encrypted = $3, error_kind = $4, \
                     finished_at = now(), due_at = NULL, lease_expires_at = NULL, \
                     awaited_event_name = NULL, awaited_event_key = NULL, awaited_sequence = NULL, \
                     consecutive_failures = 0, blocked_reason = NULL, updated_at = now() {fence}"
                ),
                vec![error.encrypted.into(), kind.into()],
                false,
            ),
            Release::Sleep { until } => (
                format!(
                    "UPDATE workflow_runs SET status = 'sleeping', due_at = $3, lease_expires_at = NULL, \
                     awaited_event_name = NULL, awaited_event_key = NULL, awaited_sequence = NULL, \
                     consecutive_failures = 0, blocked_reason = NULL, updated_at = now() {fence}"
                ),
                vec![until.into()],
                until <= Utc::now(),
            ),
            Release::Wait {
                sequence,
                event,
                key,
                timeout_at,
            } => (
                format!(
                    "UPDATE workflow_runs SET status = 'waiting', due_at = $3, lease_expires_at = NULL, \
                     awaited_event_name = $4, awaited_event_key = $5, awaited_sequence = $6, \
                     consecutive_failures = 0, blocked_reason = NULL, updated_at = now() {fence}"
                ),
                vec![timeout_at.into(), event.into(), key.into(), sequence.into()],
                false,
            ),
            Release::Retry { at } => (
                format!(
                    "UPDATE workflow_runs SET status = 'retrying', due_at = $3, lease_expires_at = NULL, \
                     awaited_event_name = NULL, awaited_event_key = NULL, awaited_sequence = NULL, \
                     consecutive_failures = 0, blocked_reason = NULL, updated_at = now() {fence}"
                ),
                vec![at.into()],
                at <= Utc::now(),
            ),
        };
        let mut values: Vec<Value> = vec![run_id.into(), lease.into()];
        values.extend(extra);
        let row = workflow_runs::Entity::find()
            .from_raw_sql(stmt(&sql, values))
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("releasing a workflow run"))?;
        match row {
            Some(row) => {
                if wake_now {
                    notify(self.db.as_ref()).await?;
                }
                Ok(row)
            }
            None => {
                let current = self.get_run(run_id).await?;
                Err(stale(current.as_ref(), run_id, lease))
            }
        }
    }

    async fn reap_expired(&self, batch: u64) -> WorkflowResult<Vec<ReapedRun>> {
        #[derive(FromQueryResult)]
        struct Reaped {
            id: Uuid,
            project_id: i32,
            environment_id: i32,
            workflow_name: String,
            status: String,
        }
        let rows = Reaped::find_by_statement(stmt(
            "WITH t AS ( \
                SELECT r.id, r.consecutive_failures + 1 AS failures, \
                       COALESCE(s.stuck_after_failures, 8) AS stuck_after \
                FROM workflow_runs r \
                LEFT JOIN workflow_environment_settings s ON s.environment_id = r.environment_id \
                WHERE r.status = 'running' AND r.lease_expires_at < now() \
                ORDER BY r.lease_expires_at LIMIT $1 \
                FOR UPDATE OF r SKIP LOCKED \
             ) \
             UPDATE workflow_runs r SET consecutive_failures = t.failures, \
                status = CASE WHEN t.failures >= t.stuck_after THEN 'stuck' \
                              WHEN r.awaited_event_name IS NOT NULL THEN 'waiting' \
                              ELSE 'retrying' END, \
                due_at = CASE WHEN t.failures >= t.stuck_after THEN NULL \
                              ELSE now() + LEAST(300, 5 * (2 ^ LEAST(t.failures - 1, 6)))::int * interval '1 second' END, \
                lease_expires_at = NULL, \
                blocked_reason = 'The invocation stopped heartbeating; its lease expired', \
                updated_at = now() \
             FROM t WHERE r.id = t.id \
             RETURNING r.id, r.project_id, r.environment_id, r.workflow_name, r.status",
            vec![i64::try_from(batch).unwrap_or(i64::MAX).into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("reaping expired workflow leases"))?;
        Ok(rows
            .into_iter()
            .map(|row| ReapedRun {
                run_id: row.id,
                project_id: row.project_id,
                environment_id: row.environment_id,
                workflow_name: row.workflow_name,
                status: RunStatus::parse(&row.status).unwrap_or(RunStatus::Retrying),
            })
            .collect())
    }

    async fn cancel_descendants(&self, run_id: Uuid) -> WorkflowResult<Vec<RunRow>> {
        // `parent_run_id` is set once, at creation, to a run that already
        // exists, so the tree has no cycles.
        workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                &format!(
                    "WITH RECURSIVE tree AS ( \
                        SELECT id FROM workflow_runs WHERE parent_run_id = $1 \
                        UNION SELECT c.id FROM workflow_runs c JOIN tree t ON c.parent_run_id = t.id \
                     ) \
                     UPDATE workflow_runs SET status = 'cancelled', error_kind = 'cancelled', \
                     finished_at = now(), due_at = NULL, lease_token = lease_token + 1, \
                     lease_expires_at = NULL, awaited_event_name = NULL, awaited_event_key = NULL, \
                     awaited_sequence = NULL, blocked_reason = NULL, updated_at = now() \
                     WHERE id IN (SELECT id FROM tree) AND status NOT IN {TERMINAL} RETURNING *"
                ),
                vec![run_id.into()],
            ))
            .all(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("cancelling the descendants of a workflow run"))
    }

    async fn cancel_run(&self, run_id: Uuid) -> WorkflowResult<Option<RunRow>> {
        workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                &format!(
                    "UPDATE workflow_runs SET status = 'cancelled', error_kind = 'cancelled', \
                     finished_at = now(), due_at = NULL, lease_token = lease_token + 1, \
                     lease_expires_at = NULL, awaited_event_name = NULL, awaited_event_key = NULL, \
                     awaited_sequence = NULL, blocked_reason = NULL, updated_at = now() \
                     WHERE id = $1 AND status NOT IN {TERMINAL} RETURNING *"
                ),
                vec![run_id.into()],
            ))
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("cancelling a workflow run"))
    }

    async fn retry_from(&self, run_id: Uuid, sequence: i32) -> WorkflowResult<Option<RunRow>> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("retrying a workflow run"))?;
        let locked = workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                "SELECT * FROM workflow_runs WHERE id = $1 FOR UPDATE",
                vec![run_id.into()],
            ))
            .one(&txn)
            .await
            .map_err(WorkflowError::db("retrying a workflow run"))?;
        let Some(run) = locked else {
            return Ok(None);
        };
        if !matches!(
            status_of(&run),
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Stuck
        ) {
            return Ok(None);
        }
        txn.execute(stmt(
            "DELETE FROM workflow_steps WHERE run_id = $1 AND sequence >= $2",
            vec![run_id.into(), sequence.into()],
        ))
        .await
        .map_err(WorkflowError::db("truncating a workflow journal"))?;
        let row = workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                "UPDATE workflow_runs r SET status = 'queued', due_at = now(), \
                 output_encrypted = NULL, output_blob_key = NULL, output_bytes = 0, \
                 error_encrypted = NULL, error_kind = NULL, finished_at = NULL, \
                 consecutive_failures = 0, blocked_reason = NULL, lease_token = r.lease_token + 1, \
                 lease_expires_at = NULL, awaited_event_name = NULL, awaited_event_key = NULL, \
                 awaited_sequence = NULL, \
                 step_count = (SELECT count(*) FROM workflow_steps s WHERE s.run_id = r.id), \
                 journal_bytes = COALESCE((SELECT sum(s.result_bytes + s.error_bytes) FROM workflow_steps s \
                     WHERE s.run_id = r.id), 0), \
                 updated_at = now() \
                 WHERE r.id = $1 RETURNING *",
                vec![run_id.into()],
            ))
            .one(&txn)
            .await
            .map_err(WorkflowError::db("retrying a workflow run"))?;
        notify(&txn).await?;
        txn.commit()
            .await
            .map_err(WorkflowError::db("retrying a workflow run"))?;
        Ok(row)
    }

    async fn resume_run(&self, run_id: Uuid) -> WorkflowResult<Option<RunRow>> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("resuming a workflow run"))?;
        let row = workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                "UPDATE workflow_runs SET status = 'queued', due_at = now(), consecutive_failures = 0, \
                 blocked_reason = NULL, updated_at = now() \
                 WHERE id = $1 AND status IN ('stuck', 'retrying', 'sleeping', 'queued') RETURNING *",
                vec![run_id.into()],
            ))
            .one(&txn)
            .await
            .map_err(WorkflowError::db("resuming a workflow run"))?;
        if row.is_some() {
            // A pending sleep wakes now: the executor compares its wake time
            // with Temps' clock on the next invocation.
            txn.execute(stmt(
                "UPDATE workflow_steps SET wake_at = now(), updated_at = now() \
                 WHERE run_id = $1 AND kind = 'sleep' AND status = 'pending'",
                vec![run_id.into()],
            ))
            .await
            .map_err(WorkflowError::db("waking a sleeping workflow run"))?;
            notify(&txn).await?;
        }
        txn.commit()
            .await
            .map_err(WorkflowError::db("resuming a workflow run"))?;
        Ok(row)
    }

    async fn delete_run(&self, run_id: Uuid) -> WorkflowResult<Option<RunRow>> {
        workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                "DELETE FROM workflow_runs WHERE id = $1 AND status <> 'running' RETURNING *",
                vec![run_id.into()],
            ))
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("deleting a workflow run"))
    }

    async fn notify_due(&self) -> WorkflowResult<()> {
        notify(self.db.as_ref()).await
    }

    async fn insert_event(&self, event: NewEvent) -> WorkflowResult<i64> {
        let row = workflow_events::ActiveModel {
            project_id: Set(event.project_id),
            environment_id: Set(event.environment_id),
            name: Set(event.name),
            event_key: Set(event.key),
            payload_encrypted: Set(event.payload.encrypted),
            payload_bytes: Set(event.payload.bytes),
            source: Set(event.source.to_string()),
            source_run_id: Set(event.source_run_id),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("storing a workflow event"))?;
        Ok(row.id)
    }

    async fn event(&self, event_id: i64) -> WorkflowResult<Option<EventRow>> {
        workflow_events::Entity::find_by_id(event_id)
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("loading a workflow event"))
    }

    async fn waiting_runs(
        &self,
        environment_id: i32,
        name: &str,
        key: &str,
        after: Option<Uuid>,
        limit: u64,
    ) -> WorkflowResult<Vec<WaitingRun>> {
        let rows = WaitingRow::find_by_statement(stmt(
            "SELECT id, project_id, environment_id, lease_token, awaited_sequence, awaited_event_name, \
             awaited_event_key FROM workflow_runs \
             WHERE environment_id = $1 AND status = 'waiting' AND awaited_event_name = $2 \
             AND awaited_event_key = $3 AND ($4::uuid IS NULL OR id > $4::uuid) \
             ORDER BY id LIMIT $5",
            vec![
                environment_id.into(),
                name.into(),
                key.into(),
                after.into(),
                i64::try_from(limit).unwrap_or(i64::MAX).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("finding runs waiting for an event"))?;
        Ok(rows
            .into_iter()
            .filter_map(WaitingRow::into_waiting)
            .collect())
    }

    async fn undelivered_waits(&self, limit: u64) -> WorkflowResult<Vec<WaitingRun>> {
        let rows = WaitingRow::find_by_statement(stmt(
            "SELECT r.id, r.project_id, r.environment_id, r.lease_token, r.awaited_sequence, r.awaited_event_name, \
             r.awaited_event_key FROM workflow_runs r \
             WHERE r.status = 'waiting' AND EXISTS ( \
                SELECT 1 FROM workflow_events e \
                WHERE e.environment_id = r.environment_id AND e.name = r.awaited_event_name \
                AND e.event_key = r.awaited_event_key AND e.created_at >= r.created_at \
                AND NOT EXISTS (SELECT 1 FROM workflow_steps s WHERE s.run_id = r.id AND s.event_id = e.id) \
             ) ORDER BY r.updated_at LIMIT $1",
            vec![i64::try_from(limit).unwrap_or(i64::MAX).into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("reconciling event deliveries"))?;
        Ok(rows
            .into_iter()
            .filter_map(WaitingRow::into_waiting)
            .collect())
    }

    async fn waits_on_finished_children(&self, limit: u64) -> WorkflowResult<Vec<WaitingRun>> {
        // The child is looked up through the parent index; comparing its id
        // as text never fails on a key that is not a UUID.
        let rows = WaitingRow::find_by_statement(stmt(
            &format!(
                "SELECT r.id, r.project_id, r.environment_id, r.lease_token, r.awaited_sequence, \
                 r.awaited_event_name, r.awaited_event_key FROM workflow_runs r \
                 WHERE r.status = 'waiting' AND r.awaited_event_name = $1 AND NOT EXISTS ( \
                    SELECT 1 FROM workflow_runs c WHERE c.parent_run_id = r.id \
                    AND c.id::text = r.awaited_event_key AND c.status NOT IN {TERMINAL} \
                 ) ORDER BY r.updated_at LIMIT $2"
            ),
            vec![
                CHILD_EVENT.into(),
                i64::try_from(limit).unwrap_or(i64::MAX).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("reconciling child workflow results"))?;
        Ok(rows
            .into_iter()
            .filter_map(WaitingRow::into_waiting)
            .collect())
    }

    async fn buffered_event(
        &self,
        run_id: Uuid,
        name: &str,
        key: &str,
    ) -> WorkflowResult<Option<EventRow>> {
        workflow_events::Entity::find()
            .from_raw_sql(stmt(
                "SELECT e.* FROM workflow_events e JOIN workflow_runs r ON r.id = $1 \
                 WHERE e.environment_id = r.environment_id AND e.name = $2 AND e.event_key = $3 \
                 AND e.created_at >= r.created_at \
                 AND NOT EXISTS (SELECT 1 FROM workflow_steps s WHERE s.run_id = r.id AND s.event_id = e.id) \
                 ORDER BY e.id LIMIT 1",
                vec![run_id.into(), name.into(), key.into()],
            ))
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("looking for a buffered workflow event"))
    }

    async fn complete_wait(
        &self,
        waiting: &WaitingRun,
        event_id: Option<i64>,
        result: StoredPayload,
        max_journal_bytes: i64,
    ) -> WorkflowResult<WaitCompletion> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("delivering a workflow event"))?;
        // The run's lock serialises this with claims, cancels, retries and
        // other deliveries: what is checked below stays true until commit.
        let run = workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                "SELECT * FROM workflow_runs WHERE id = $1 FOR UPDATE",
                vec![waiting.run_id.into()],
            ))
            .one(&txn)
            .await
            .map_err(WorkflowError::db("delivering a workflow event"))?;
        let suspended = run.as_ref().is_some_and(|run| {
            status_of(run) == RunStatus::Waiting
                && run.lease_token == waiting.lease
                && run.awaited_sequence == Some(waiting.sequence)
                && run.awaited_event_name.as_deref() == Some(waiting.event_name.as_str())
                && run.awaited_event_key.as_deref().unwrap_or_default() == waiting.event_key
        });
        let Some(run) = run.filter(|_| suspended) else {
            return Ok(WaitCompletion::NotWaiting);
        };
        if let Some(event_id) = event_id {
            // One event satisfies one wait of a run, and only an event the
            // run received after it started (as `buffered_event` picks).
            let usable = txn
                .query_one(stmt(
                    "SELECT 1 AS usable FROM workflow_events e WHERE e.id = $2 \
                     AND e.created_at >= $3 \
                     AND NOT EXISTS (SELECT 1 FROM workflow_steps s \
                                     WHERE s.run_id = $1 AND s.event_id = e.id)",
                    vec![
                        waiting.run_id.into(),
                        event_id.into(),
                        run.created_at.into(),
                    ],
                ))
                .await
                .map_err(WorkflowError::db("delivering a workflow event"))?;
            if usable.is_none() {
                return Ok(WaitCompletion::NotWaiting);
            }
        }
        let step = workflow_steps::Entity::find_by_id((waiting.run_id, waiting.sequence))
            .one(&txn)
            .await
            .map_err(WorkflowError::db("delivering a workflow event"))?;
        // A child's result completes the `child` entry; an event, a `wait`.
        let kind = if waiting.event_name == CHILD_EVENT {
            StepKind::Child
        } else {
            StepKind::Wait
        };
        let Some(step) = step.filter(|step| {
            step.kind == kind.as_str() && step.status == StepStatus::Pending.as_str()
        }) else {
            return Ok(WaitCompletion::NotWaiting);
        };
        let delta =
            i64::from(result.bytes) - i64::from(step.result_bytes) - i64::from(step.error_bytes);
        if run.journal_bytes + delta > max_journal_bytes {
            return Ok(WaitCompletion::OverQuota {
                journal_bytes: run.journal_bytes,
                result_bytes: result.bytes,
            });
        }
        let blob = result.blob_key.is_some();
        txn.execute(stmt(
            "UPDATE workflow_steps SET status = 'completed', result_encrypted = $3, \
             result_blob_key = $4, result_bytes = $5, error_encrypted = NULL, error_bytes = 0, \
             event_id = $6, completed_at = now(), updated_at = now() \
             WHERE run_id = $1 AND sequence = $2",
            vec![
                waiting.run_id.into(),
                waiting.sequence.into(),
                result.encrypted.into(),
                result.blob_key.into(),
                result.bytes.into(),
                event_id.into(),
            ],
        ))
        .await
        .map_err(WorkflowError::db("delivering a workflow event"))?;
        txn.execute(stmt(
            "UPDATE workflow_runs SET journal_bytes = journal_bytes + $2, \
             has_blob_payloads = has_blob_payloads OR $3, \
             status = 'queued', due_at = now(), \
             awaited_event_name = NULL, awaited_event_key = NULL, awaited_sequence = NULL, \
             updated_at = now() WHERE id = $1",
            vec![waiting.run_id.into(), delta.into(), blob.into()],
        ))
        .await
        .map_err(WorkflowError::db("waking a workflow run"))?;
        notify(&txn).await?;
        txn.commit()
            .await
            .map_err(WorkflowError::db("delivering a workflow event"))?;
        Ok(WaitCompletion::Completed)
    }

    async fn fail_wait(
        &self,
        waiting: &WaitingRun,
        error: StoredPayload,
        kind: &'static str,
    ) -> WorkflowResult<Option<String>> {
        let row = self
            .db
            .query_one(stmt(
                "UPDATE workflow_runs SET status = 'failed', error_encrypted = $4, error_kind = $5, \
                 finished_at = now(), due_at = NULL, lease_expires_at = NULL, \
                 awaited_event_name = NULL, awaited_event_key = NULL, awaited_sequence = NULL, \
                 consecutive_failures = 0, blocked_reason = NULL, updated_at = now() \
                 WHERE id = $1 AND lease_token = $2 AND awaited_sequence = $3 AND status = 'waiting' \
                 RETURNING workflow_name",
                vec![
                    waiting.run_id.into(),
                    waiting.lease.into(),
                    waiting.sequence.into(),
                    error.encrypted.into(),
                    kind.into(),
                ],
            ))
            .await
            .map_err(WorkflowError::db("failing a waiting workflow run"))?;
        row.map(|row| row.try_get::<String>("", "workflow_name"))
            .transpose()
            .map_err(WorkflowError::db("failing a waiting workflow run"))
    }

    async fn record_step(
        &self,
        record: StepRecord,
        limits: StepLimits,
    ) -> WorkflowResult<StepRecordOutcome> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(WorkflowError::db("recording a journal entry"))?;
        let run = workflow_runs::Entity::find()
            .from_raw_sql(stmt(
                "SELECT * FROM workflow_runs WHERE id = $1 FOR UPDATE",
                vec![record.run_id.into()],
            ))
            .one(&txn)
            .await
            .map_err(WorkflowError::db("recording a journal entry"))?;
        let Some(run) = run else {
            return Err(stale(None, record.run_id, record.lease));
        };
        if status_of(&run) != RunStatus::Running || run.lease_token != record.lease {
            return Err(stale(Some(&run), record.run_id, record.lease));
        }
        let existing = workflow_steps::Entity::find_by_id((record.run_id, record.sequence))
            .one(&txn)
            .await
            .map_err(WorkflowError::db("recording a journal entry"))?;
        let new_bytes = i64::from(record.result.bytes) + i64::from(record.error.bytes);
        let blob = record.result.blob_key.is_some() || record.error.blob_key.is_some();
        let (entry, inserted, bytes_delta, already) = match existing {
            Some(existing) => {
                if existing.name != record.name || existing.kind != record.kind.as_str() {
                    return Err(WorkflowError::Incompatible {
                        run_id: record.run_id,
                        sequence: record.sequence,
                        expected: existing.name,
                        found: record.name,
                    });
                }
                if StepStatus::parse(&existing.status).is_some_and(StepStatus::is_final) {
                    (existing, false, 0, true)
                } else {
                    let old_bytes =
                        i64::from(existing.result_bytes) + i64::from(existing.error_bytes);
                    if run.journal_bytes + new_bytes - old_bytes > limits.max_journal_bytes {
                        return Err(WorkflowError::QuotaExceeded {
                            environment_id: run.environment_id,
                            quota: "max_journal_bytes_per_run",
                            limit: limits.max_journal_bytes,
                        });
                    }
                    let mut active: workflow_steps::ActiveModel = existing.into();
                    active.status = Set(record.status.as_str().to_string());
                    active.attempts = Set(record.attempts);
                    active.result_encrypted = Set(record.result.encrypted.clone());
                    active.result_blob_key = Set(record.result.blob_key.clone());
                    active.result_bytes = Set(record.result.bytes);
                    active.error_encrypted = Set(record.error.encrypted.clone());
                    active.error_bytes = Set(record.error.bytes);
                    active.lease_token = Set(record.lease);
                    if record.wake_at.is_some() {
                        active.wake_at = Set(record.wake_at);
                    }
                    active.duration_ms = Set(record.duration_ms);
                    active.trace_id = Set(record.trace_id.clone());
                    active.error_ref = Set(record.error_ref.clone());
                    if record.status.is_final() {
                        active.completed_at = Set(Some(Utc::now()));
                    }
                    let updated = active
                        .update(&txn)
                        .await
                        .map_err(WorkflowError::db("updating a journal entry"))?;
                    (updated, false, new_bytes - old_bytes, false)
                }
            }
            None => {
                if record.sequence != run.step_count {
                    return Err(WorkflowError::Validation {
                        field: "sequence",
                        reason: format!(
                            "entry {} of run {} is not the next one; the journal has {} entries",
                            record.sequence, record.run_id, run.step_count
                        ),
                    });
                }
                if run.step_count >= limits.max_steps {
                    return Err(WorkflowError::QuotaExceeded {
                        environment_id: run.environment_id,
                        quota: "max_steps_per_run",
                        limit: limits.max_steps.into(),
                    });
                }
                if run.journal_bytes + new_bytes > limits.max_journal_bytes {
                    return Err(WorkflowError::QuotaExceeded {
                        environment_id: run.environment_id,
                        quota: "max_journal_bytes_per_run",
                        limit: limits.max_journal_bytes,
                    });
                }
                let now = Utc::now();
                let inserted = workflow_steps::ActiveModel {
                    run_id: Set(record.run_id),
                    sequence: Set(record.sequence),
                    name: Set(record.name.clone()),
                    kind: Set(record.kind.as_str().to_string()),
                    status: Set(record.status.as_str().to_string()),
                    attempts: Set(record.attempts),
                    result_encrypted: Set(record.result.encrypted.clone()),
                    result_blob_key: Set(record.result.blob_key.clone()),
                    result_bytes: Set(record.result.bytes),
                    error_encrypted: Set(record.error.encrypted.clone()),
                    error_bytes: Set(record.error.bytes),
                    lease_token: Set(record.lease),
                    event_id: Set(None),
                    wake_at: Set(record.wake_at),
                    duration_ms: Set(record.duration_ms),
                    trace_id: Set(record.trace_id.clone()),
                    error_ref: Set(record.error_ref.clone()),
                    completed_at: Set(record.status.is_final().then_some(now)),
                    ..Default::default()
                }
                .insert(&txn)
                .await
                .map_err(WorkflowError::db("inserting a journal entry"))?;
                (inserted, true, new_bytes, false)
            }
        };
        let mut event_id = None;
        if inserted {
            if let Some(event) = record.event {
                let row = workflow_events::ActiveModel {
                    project_id: Set(event.project_id),
                    environment_id: Set(event.environment_id),
                    name: Set(event.name),
                    event_key: Set(event.key),
                    payload_encrypted: Set(event.payload.encrypted),
                    payload_bytes: Set(event.payload.bytes),
                    source: Set(event.source.to_string()),
                    source_run_id: Set(event.source_run_id),
                    ..Default::default()
                }
                .insert(&txn)
                .await
                .map_err(WorkflowError::db("storing an event sent by a workflow"))?;
                event_id = Some(row.id);
            }
        }
        let row = txn
            .query_one(stmt(
                "UPDATE workflow_runs SET step_count = step_count + $2, \
                 journal_bytes = journal_bytes + $3, has_blob_payloads = has_blob_payloads OR $4, \
                 consecutive_failures = 0, \
                 lease_expires_at = now() + $5 * interval '1 second', updated_at = now() \
                 WHERE id = $1 RETURNING lease_expires_at",
                vec![
                    record.run_id.into(),
                    i32::from(inserted).into(),
                    bytes_delta.into(),
                    blob.into(),
                    limits.lease_secs.into(),
                ],
            ))
            .await
            .map_err(WorkflowError::db("extending a workflow lease"))?;
        let lease_expires_at = row
            .map(|row| row.try_get::<DateTime<Utc>>("", "lease_expires_at"))
            .transpose()
            .map_err(WorkflowError::db("extending a workflow lease"))?
            .unwrap_or_else(Utc::now);
        txn.commit()
            .await
            .map_err(WorkflowError::db("recording a journal entry"))?;
        Ok(if already {
            StepRecordOutcome::AlreadyRecorded {
                entry,
                lease_expires_at,
            }
        } else {
            StepRecordOutcome::Recorded {
                entry,
                lease_expires_at,
                event_id,
            }
        })
    }

    async fn journal(
        &self,
        run_id: Uuid,
        from_sequence: i32,
        limit: u64,
    ) -> WorkflowResult<Vec<StepRow>> {
        workflow_steps::Entity::find()
            .filter(workflow_steps::Column::RunId.eq(run_id))
            .filter(workflow_steps::Column::Sequence.gte(from_sequence))
            .order_by_asc(workflow_steps::Column::Sequence)
            .limit(limit)
            .all(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("reading a workflow journal"))
    }

    async fn step(&self, run_id: Uuid, sequence: i32) -> WorkflowResult<Option<StepRow>> {
        workflow_steps::Entity::find_by_id((run_id, sequence))
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("reading a journal entry"))
    }

    async fn get_run(&self, run_id: Uuid) -> WorkflowResult<Option<RunRow>> {
        workflow_runs::Entity::find_by_id(run_id)
            .one(self.db.as_ref())
            .await
            .map_err(WorkflowError::db("loading a workflow run"))
    }

    async fn list_runs(
        &self,
        environment_id: i32,
        filter: &RunFilter,
        page: u64,
        page_size: u64,
    ) -> WorkflowResult<(Vec<RunRow>, u64)> {
        let mut query = workflow_runs::Entity::find()
            .filter(workflow_runs::Column::EnvironmentId.eq(environment_id));
        if let Some(status) = filter.status {
            query = query.filter(workflow_runs::Column::Status.eq(status.as_str()));
        }
        if let Some(name) = &filter.workflow_name {
            query = query.filter(workflow_runs::Column::WorkflowName.eq(name.clone()));
        }
        let paginator = query
            .order_by_desc(workflow_runs::Column::CreatedAt)
            .order_by_desc(workflow_runs::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator
            .num_items()
            .await
            .map_err(WorkflowError::db("counting workflow runs"))?;
        let rows = paginator
            .fetch_page(page.saturating_sub(1))
            .await
            .map_err(WorkflowError::db("listing workflow runs"))?;
        Ok((rows, total))
    }

    async fn workflow_summaries(
        &self,
        environment_id: i32,
    ) -> WorkflowResult<HashMap<String, WorkflowSummary>> {
        #[derive(FromQueryResult)]
        struct Row {
            workflow_name: String,
            active: i64,
            completed: i64,
            failed: i64,
            last_run_at: Option<DateTime<Utc>>,
        }
        let rows = Row::find_by_statement(stmt(
            &format!(
                "SELECT workflow_name, \
                 count(*) FILTER (WHERE status NOT IN {TERMINAL})::BIGINT AS active, \
                 count(*) FILTER (WHERE status = 'completed')::BIGINT AS completed, \
                 count(*) FILTER (WHERE status = 'failed')::BIGINT AS failed, \
                 max(created_at) AS last_run_at \
                 FROM workflow_runs WHERE environment_id = $1 GROUP BY workflow_name \
                 ORDER BY workflow_name LIMIT 1000"
            ),
            vec![environment_id.into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("summarizing workflows"))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.workflow_name.clone(),
                    WorkflowSummary {
                        workflow_name: row.workflow_name,
                        active: row.active,
                        completed: row.completed,
                        failed: row.failed,
                        last_run_at: row.last_run_at,
                    },
                )
            })
            .collect())
    }

    async fn queue_stats(&self) -> WorkflowResult<Vec<QueueStat>> {
        #[derive(FromQueryResult)]
        struct Row {
            project_id: i32,
            environment_id: i32,
            workflow_name: String,
            depth: i64,
            oldest_due_at: Option<DateTime<Utc>>,
            stuck: i64,
        }
        let rows = Row::find_by_statement(stmt(
            &format!(
                "SELECT project_id, environment_id, workflow_name, \
                 count(*) FILTER (WHERE status IN {RUNNABLE} AND due_at <= now())::BIGINT AS depth, \
                 min(due_at) FILTER (WHERE status IN {RUNNABLE} AND due_at <= now()) AS oldest_due_at, \
                 count(*) FILTER (WHERE status = 'stuck')::BIGINT AS stuck \
                 FROM workflow_runs \
                 WHERE (status IN {RUNNABLE} AND due_at <= now()) OR status = 'stuck' \
                 GROUP BY project_id, environment_id, workflow_name LIMIT 5000"
            ),
            vec![],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("measuring workflow queues"))?;
        Ok(rows
            .into_iter()
            .map(|row| QueueStat {
                project_id: row.project_id,
                environment_id: row.environment_id,
                workflow_name: row.workflow_name,
                depth: row.depth,
                oldest_due_at: row.oldest_due_at,
                stuck: row.stuck,
            })
            .collect())
    }

    async fn journal_sizes(&self) -> WorkflowResult<Vec<JournalSize>> {
        #[derive(FromQueryResult)]
        struct Row {
            project_id: i32,
            environment_id: i32,
            bytes: i64,
        }
        let rows = Row::find_by_statement(stmt(
            "SELECT project_id, environment_id, \
             sum(journal_bytes + input_bytes + output_bytes)::BIGINT AS bytes \
             FROM workflow_runs GROUP BY project_id, environment_id",
            vec![],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("measuring workflow storage"))?;
        Ok(rows
            .into_iter()
            .map(|row| JournalSize {
                project_id: row.project_id,
                environment_id: row.environment_id,
                bytes: row.bytes,
            })
            .collect())
    }

    async fn delete_expired_runs(&self, batch: u64) -> WorkflowResult<Vec<ExpiredRun>> {
        #[derive(FromQueryResult)]
        struct Row {
            id: Uuid,
            project_id: i32,
            environment_id: i32,
            has_blob_payloads: bool,
        }
        // One statement: the row is locked and its expiry checked again as it
        // is deleted, so a run retried after it was picked (queued again,
        // finished_at cleared) is skipped rather than deleted, and the caller
        // removes the blobs only of runs that are gone.
        let rows = Row::find_by_statement(stmt(
            &format!(
                "DELETE FROM workflow_runs r USING ( \
                     SELECT c.id FROM workflow_runs c \
                     LEFT JOIN workflow_environment_settings s ON s.environment_id = c.environment_id \
                     WHERE c.finished_at IS NOT NULL AND c.status IN {TERMINAL} \
                     AND c.finished_at < now() - COALESCE(s.retention_days, 30) * interval '1 day' \
                     ORDER BY c.finished_at LIMIT $1 FOR UPDATE OF c SKIP LOCKED \
                 ) expired \
                 WHERE r.id = expired.id AND r.finished_at IS NOT NULL AND r.status IN {TERMINAL} \
                 AND r.finished_at < now() - COALESCE( \
                     (SELECT s.retention_days FROM workflow_environment_settings s \
                      WHERE s.environment_id = r.environment_id), 30) * interval '1 day' \
                 RETURNING r.id, r.project_id, r.environment_id, r.has_blob_payloads"
            ),
            vec![i64::try_from(batch).unwrap_or(i64::MAX).into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(WorkflowError::db("deleting expired workflow runs"))?;
        Ok(rows
            .into_iter()
            .map(|row| ExpiredRun {
                run_id: row.id,
                project_id: row.project_id,
                environment_id: row.environment_id,
                has_blob_payloads: row.has_blob_payloads,
            })
            .collect())
    }

    async fn delete_expired_events(&self, batch: u64) -> WorkflowResult<u64> {
        let result = self
            .db
            .execute(stmt(
                "DELETE FROM workflow_events WHERE id IN ( \
                    SELECT e.id FROM workflow_events e \
                    LEFT JOIN workflow_environment_settings s ON s.environment_id = e.environment_id \
                    WHERE e.created_at < now() - COALESCE(s.retention_days, 30) * interval '1 day' \
                    ORDER BY e.id LIMIT $1)",
                vec![i64::try_from(batch).unwrap_or(i64::MAX).into()],
            ))
            .await
            .map_err(WorkflowError::db("deleting expired workflow events"))?;
        Ok(result.rows_affected())
    }
}
