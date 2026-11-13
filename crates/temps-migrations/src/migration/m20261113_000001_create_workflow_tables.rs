// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable workflows (ADR-103): run state, the journal, events, the
//! per-environment settings row and the app credential.
//!
//! New tables only, so the previous binary keeps working on this schema. The
//! tables live in the default schema with a `workflow_` prefix rather than in a
//! separate Postgres schema (ADR-103, "Verified against the code", item 13).
//! `workflow_steps` is a plain table, not a hypertable (item 12): it is updated
//! in place, keyed `(run_id, sequence)` for idempotent journal writes, and lives
//! exactly as long as its run.
//!
//! An environment without a `workflow_environment_settings` row uses the
//! column defaults below; the row only exists once someone changes a setting,
//! so there is no backfill. `managed_store_service_id` is reserved for the
//! opt-in managed Postgres journal (M3) and unused until then.
//!
//! Payload columns (`*_encrypted`) hold AES-256-GCM ciphertext under
//! associated data that binds them to their run, step and field; a `*_blob_key`
//! is set instead when the ciphertext was offloaded to blob storage.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_environment_settings (
    environment_id INTEGER PRIMARY KEY REFERENCES environments(id) ON DELETE CASCADE,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    host_service TEXT NULL CHECK (host_service IS NULL OR char_length(host_service) BETWEEN 1 AND 128),
    invocation_deadline_secs INTEGER NOT NULL DEFAULT 900 CHECK (invocation_deadline_secs BETWEEN 10 AND 3600),
    lease_secs INTEGER NOT NULL DEFAULT 120 CHECK (lease_secs BETWEEN 15 AND 900),
    max_concurrent_runs INTEGER NOT NULL DEFAULT 50 CHECK (max_concurrent_runs BETWEEN 1 AND 10000),
    max_active_runs INTEGER NOT NULL DEFAULT 10000 CHECK (max_active_runs BETWEEN 1 AND 1000000),
    max_steps_per_run INTEGER NOT NULL DEFAULT 1000 CHECK (max_steps_per_run BETWEEN 1 AND 100000),
    max_journal_bytes_per_run BIGINT NOT NULL DEFAULT 67108864 CHECK (max_journal_bytes_per_run BETWEEN 1024 AND 1073741824),
    inline_payload_limit_bytes INTEGER NOT NULL DEFAULT 262144 CHECK (inline_payload_limit_bytes BETWEEN 1024 AND 1048576),
    max_payload_bytes INTEGER NOT NULL DEFAULT 4194304 CHECK (max_payload_bytes BETWEEN 1024 AND 67108864),
    retention_days INTEGER NOT NULL DEFAULT 30 CHECK (retention_days BETWEEN 1 AND 3650),
    stuck_after_failures INTEGER NOT NULL DEFAULT 8 CHECK (stuck_after_failures BETWEEN 1 AND 100),
    managed_store_service_id INTEGER NULL REFERENCES external_services(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS workflow_environment_settings_project
    ON workflow_environment_settings(project_id);

CREATE TABLE IF NOT EXISTS workflow_credentials (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    token_prefix TEXT NOT NULL,
    created_by_user_id INTEGER NULL REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ NULL,
    revoked_at TIMESTAMPTZ NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS workflow_credentials_one_active
    ON workflow_credentials(environment_id) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS workflow_credentials_project ON workflow_credentials(project_id);

CREATE TABLE IF NOT EXISTS workflow_runs (
    id UUID PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    workflow_name TEXT NOT NULL CHECK (char_length(workflow_name) BETWEEN 1 AND 128),
    status TEXT NOT NULL CHECK (status IN (
        'queued', 'running', 'sleeping', 'waiting', 'retrying',
        'completed', 'failed', 'cancelled', 'stuck'
    )),
    input_encrypted TEXT NULL,
    input_blob_key TEXT NULL,
    input_bytes INTEGER NOT NULL DEFAULT 0,
    output_encrypted TEXT NULL,
    output_blob_key TEXT NULL,
    output_bytes INTEGER NOT NULL DEFAULT 0,
    error_encrypted TEXT NULL,
    error_kind TEXT NULL CHECK (error_kind IS NULL OR char_length(error_kind) <= 64),
    idempotency_key TEXT NULL CHECK (idempotency_key IS NULL OR char_length(idempotency_key) BETWEEN 1 AND 256),
    parent_run_id UUID NULL REFERENCES workflow_runs(id) ON DELETE SET NULL,
    started_by_credential_id INTEGER NULL REFERENCES workflow_credentials(id) ON DELETE SET NULL,
    started_by_user_id INTEGER NULL REFERENCES users(id) ON DELETE SET NULL,
    deployment_id INTEGER NULL REFERENCES deployments(id) ON DELETE SET NULL,
    attempt INTEGER NOT NULL DEFAULT 0,
    lease_token BIGINT NOT NULL DEFAULT 0,
    lease_expires_at TIMESTAMPTZ NULL,
    due_at TIMESTAMPTZ NULL,
    awaited_event_name TEXT NULL,
    awaited_event_key TEXT NULL,
    awaited_sequence INTEGER NULL,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    blocked_reason TEXT NULL,
    journal_bytes BIGINT NOT NULL DEFAULT 0,
    step_count INTEGER NOT NULL DEFAULT 0,
    has_blob_payloads BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ NULL,
    finished_at TIMESTAMPTZ NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS workflow_runs_idempotency
    ON workflow_runs(environment_id, workflow_name, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- The scheduler's claim: O(due rows), never a scan of finished runs.
CREATE INDEX IF NOT EXISTS workflow_runs_due ON workflow_runs(due_at)
    WHERE status IN ('queued', 'sleeping', 'waiting', 'retrying');
CREATE INDEX IF NOT EXISTS workflow_runs_leases ON workflow_runs(lease_expires_at)
    WHERE status = 'running';
CREATE INDEX IF NOT EXISTS workflow_runs_environment_created
    ON workflow_runs(environment_id, created_at DESC);
CREATE INDEX IF NOT EXISTS workflow_runs_waiting_event
    ON workflow_runs(environment_id, awaited_event_name, awaited_event_key)
    WHERE status = 'waiting';
CREATE INDEX IF NOT EXISTS workflow_runs_finished ON workflow_runs(finished_at)
    WHERE finished_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS workflow_runs_project ON workflow_runs(project_id);
-- Counting active runs for `max_active_runs` reads only active rows.
CREATE INDEX IF NOT EXISTS workflow_runs_active ON workflow_runs(environment_id)
    WHERE status NOT IN ('completed', 'failed', 'cancelled');
CREATE INDEX IF NOT EXISTS workflow_runs_parent ON workflow_runs(parent_run_id)
    WHERE parent_run_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS workflow_steps (
    run_id UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 256),
    kind TEXT NOT NULL CHECK (kind IN ('step', 'sleep', 'wait', 'now', 'random', 'send_event')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'completed', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    result_encrypted TEXT NULL,
    result_blob_key TEXT NULL,
    result_bytes INTEGER NOT NULL DEFAULT 0,
    error_encrypted TEXT NULL,
    error_bytes INTEGER NOT NULL DEFAULT 0,
    lease_token BIGINT NOT NULL,
    event_id BIGINT NULL,
    wake_at TIMESTAMPTZ NULL,
    duration_ms BIGINT NULL,
    trace_id TEXT NULL CHECK (trace_id IS NULL OR char_length(trace_id) <= 64),
    error_ref TEXT NULL CHECK (error_ref IS NULL OR char_length(error_ref) <= 128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ NULL,
    PRIMARY KEY (run_id, sequence)
);

CREATE TABLE IF NOT EXISTS workflow_events (
    id BIGSERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    event_key TEXT NOT NULL DEFAULT '' CHECK (char_length(event_key) <= 256),
    payload_encrypted TEXT NULL,
    payload_bytes INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL CHECK (source IN ('api', 'credential', 'workflow')),
    source_run_id UUID NULL REFERENCES workflow_runs(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS workflow_events_lookup
    ON workflow_events(environment_id, name, event_key, created_at);
CREATE INDEX IF NOT EXISTS workflow_events_created ON workflow_events(created_at);
CREATE INDEX IF NOT EXISTS workflow_events_project ON workflow_events(project_id);
CREATE INDEX IF NOT EXISTS workflow_events_source_run ON workflow_events(source_run_id)
    WHERE source_run_id IS NOT NULL;
"#;

pub const DOWN: &str = r#"
DROP TABLE IF EXISTS workflow_events;
DROP TABLE IF EXISTS workflow_steps;
DROP TABLE IF EXISTS workflow_runs;
DROP TABLE IF EXISTS workflow_credentials;
DROP TABLE IF EXISTS workflow_environment_settings;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    /// Drops the workflow tables, but refuses while any run or event exists:
    /// they are user data (journals of work in progress), and rolling the
    /// feature back must not silently delete them. Delete the runs through the
    /// API first.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT (SELECT count(*) FROM workflow_runs)::BIGINT AS runs, \
                        (SELECT count(*) FROM workflow_events)::BIGINT AS events"
                    .to_string(),
            ))
            .await?;
        if let Some(row) = row {
            let runs: i64 = row.try_get("", "runs")?;
            let events: i64 = row.try_get("", "events")?;
            if runs > 0 || events > 0 {
                return Err(DbErr::Migration(format!(
                    "Refusing to drop the workflow tables: {runs} workflow runs and {events} \
                     workflow events still exist. Delete them through the workflows API \
                     before rolling back m20261113_000001_create_workflow_tables."
                )));
            }
        }
        db.execute_unprepared(DOWN).await?;
        Ok(())
    }
}
