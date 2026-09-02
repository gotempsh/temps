// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-042 §8: the bulk Cloud-telemetry activation job and its per-project
//! rows.
//!
//! Switching one project to Cloud-primary is a single `PATCH`; backfilling one
//! project's history is a single CLI invocation. An operator with 40 projects
//! has neither — they have a migration project. These two tables are the
//! durable state of the one bulk-activation engine that turns that into one
//! action, and they are the **only** source of truth for resuming it.
//!
//! # Why the cursor lives here
//!
//! ADR-042 P0 gave the backfill its own [`SubmissionScope`], which isolates its
//! counters from the live mirror — but a scoped flush deliberately does *not*
//! write anything resumable into the Cloud link's `state.json` (that file is a
//! credential store, and rewriting it per batch would be a second, weaker copy
//! of a cursor the caller already owns). So the caller has to own one, and for
//! the bulk worker "the caller" is this table:
//! `resume_start_time` / `resume_row_id` / `resume_span_id` are exactly
//! `CloudBackfillCursor`'s three fields, persisted after every completed chunk.
//! A kill mid-job therefore costs at most the chunk in flight, and a restart
//! re-enters `backfill_cloud_telemetry_window` with the same cursor the CLI's
//! `--resume` flag would have passed — no re-shipping, no lost position.
//!
//! # At most one active job
//!
//! ADR-042 §8: "At most one job may be `running` at a time; a second request
//! while one is running returns the in-flight job's id rather than queueing a
//! competing one." That is enforced here by a partial unique index rather than
//! only in the service, because submission concurrency is 1 *globally*
//! (ADR-041 §3b) and a second job would deadlock on the submission scope while
//! spending the customer's money out of order. The indexed expression
//! (`status IS NOT NULL`) is constant-true for a `NOT NULL` column, so
//! uniqueness over the partial set means "at most one row in an active state".
//!
//! # Additive, and inert without a Cloud link
//!
//! Both tables start empty and are written only by the bulk-activation worker,
//! which is spawned only when the instance has a Cloud link. The one change to
//! an existing table is a nullable `bulk_job_id` column on
//! `cloud_telemetry_backfills`, which preserves that table's `UNIQUE
//! (project_id)` — a project has one live backfill, whoever started it — and
//! lets the per-project progress surface say *which* bulk job is driving it
//! instead of appearing to be "already running" for no visible reason. An
//! instance with no Cloud link is behaviourally unchanged, in the same sense
//! ADR-041 §4 asserts.
//!
//! `project_id` deliberately carries no foreign key, matching
//! `cloud_span_outbox` and `project_telemetry_write_intervals`: deleting a
//! project must not be blocked by, or cascade into, telemetry bookkeeping. The
//! worker treats a project row that no longer resolves as a `skipped` project
//! with reason `project_not_found`, which is information rather than a
//! constraint violation.

use sea_orm_migration::prelude::*;

/// Kept in sync with `temps_entities::cloud_telemetry_bulk_jobs::BulkJobStatus`.
const JOB_STATUSES: &str =
    "'pending', 'running', 'completed', 'completed_with_failures', 'aborted', 'cancelled'";

/// Kept in sync with `temps_entities::cloud_telemetry_bulk_jobs::BulkJobTrigger`.
///
/// Both ADR-042 entry points are modelled now even though P1 has no HTTP layer
/// to create either one, so P2 (operator path) and P3 (purchase path) do not
/// each need a migration to add a value to a `CHECK`.
const JOB_TRIGGERS: &str = "'purchase', 'operator'";

/// Kept in sync with
/// `temps_entities::cloud_telemetry_bulk_job_projects::BulkJobProjectStatus`.
const PROJECT_STATUSES: &str = "'pending', 'switching', 'backfilling', 'done', 'failed', 'skipped'";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();

        connection
            .execute_unprepared(&format!(
                r#"
CREATE TABLE IF NOT EXISTS cloud_telemetry_bulk_jobs (
    id UUID PRIMARY KEY,
    -- Which entry point created this job (ADR-042 §1). The two differ only in
    -- whether a human confirmed the estimate, which is why the engine below
    -- does not branch on it at all.
    "trigger" TEXT NOT NULL,
    -- NULL on the purchase path: there is no operator, the payment is the
    -- authorization. ON DELETE SET NULL so removing a user never destroys the
    -- record of a spend.
    requested_by_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    -- Pre-send figures, summed from the per-project rows. Recorded on both
    -- paths: payment changes who authorizes a spend, not whether it is
    -- estimated and auditable.
    estimated_spans BIGINT NOT NULL DEFAULT 0,
    estimated_bytes BIGINT NOT NULL DEFAULT 0,
    -- Post-send actuals, advanced as each chunk is acknowledged. Durable so a
    -- restart resumes with the correct running total rather than restarting the
    -- count at zero.
    spans_shipped BIGINT NOT NULL DEFAULT 0,
    bytes_shipped BIGINT NOT NULL DEFAULT 0,
    -- Set only on the operator path, where a `plan_token` binds the confirmed
    -- estimate to the exact project set and windows (ADR-042 §9).
    plan_hash TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    -- Honoured at the next chunk boundary. The cursor is durable, so cancel is
    -- clean: no partial batch, no lost position (ADR-042 §7).
    cancel_requested_at TIMESTAMPTZ,
    -- The single actionable reason an instance-wide failure stopped the whole
    -- job, rather than 23 duplicates of it spread across 23 project rows.
    abort_reason TEXT,
    CONSTRAINT cloud_telemetry_bulk_jobs_trigger_valid
        CHECK ("trigger" IN ({JOB_TRIGGERS})),
    CONSTRAINT cloud_telemetry_bulk_jobs_status_valid
        CHECK (status IN ({JOB_STATUSES}))
);
"#
            ))
            .await?;

        // ADR-042 §8. `status IS NOT NULL` is constant-true for this NOT NULL
        // column, so this reads as "at most one row whose status is pending or
        // running". Enforced in the schema and not only in the service because
        // two concurrent jobs would contend for a submission scope that is
        // exclusive process-wide, and the loser would fail in a way that costs
        // money to diagnose.
        connection
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS cloud_telemetry_bulk_jobs_one_active \
                 ON cloud_telemetry_bulk_jobs ((status IS NOT NULL)) \
                 WHERE status IN ('pending', 'running')",
            )
            .await?;

        connection
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_telemetry_bulk_jobs_created \
                 ON cloud_telemetry_bulk_jobs (created_at DESC)",
            )
            .await?;

        connection
            .execute_unprepared(&format!(
                r#"
CREATE TABLE IF NOT EXISTS cloud_telemetry_bulk_job_projects (
    id BIGSERIAL PRIMARY KEY,
    job_id UUID NOT NULL REFERENCES cloud_telemetry_bulk_jobs(id) ON DELETE CASCADE,
    -- No foreign key, matching `cloud_span_outbox` and the write-mode ledger:
    -- deleting a project must not be blocked by telemetry bookkeeping. The
    -- worker records an unresolvable project as skipped with a reason.
    project_id INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    -- Machine-readable, e.g. `fidelity_not_queryable` (ADR-042 §4). A skip is
    -- never silent and never auto-fixed: raising a project's fidelity is a
    -- separate decision with its own cost.
    skip_reason TEXT,
    window_from TIMESTAMPTZ NOT NULL,
    window_to TIMESTAMPTZ NOT NULL,
    estimated_spans BIGINT NOT NULL DEFAULT 0,
    estimated_bytes BIGINT NOT NULL DEFAULT 0,
    spans_shipped BIGINT NOT NULL DEFAULT 0,
    bytes_shipped BIGINT NOT NULL DEFAULT 0,
    -- `CloudBackfillCursor`, persisted after every completed chunk. This is the
    -- resume position; see the module docs for why it cannot live in the Cloud
    -- link's state file.
    resume_start_time TIMESTAMPTZ,
    resume_row_id BIGINT,
    resume_span_id TEXT,
    -- Truncated at the same ceiling the per-project progress surface uses, so
    -- an unbounded driver message cannot be republished through a read API.
    last_error TEXT,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    CONSTRAINT cloud_telemetry_bulk_job_projects_unique UNIQUE (job_id, project_id),
    CONSTRAINT cloud_telemetry_bulk_job_projects_status_valid
        CHECK (status IN ({PROJECT_STATUSES})),
    CONSTRAINT cloud_telemetry_bulk_job_projects_window_ordered
        CHECK (window_to >= window_from)
);
"#
            ))
            .await?;

        // The worker's one hot query: "the lowest-numbered project in this job
        // that is still pending". Ascending project id is the documented
        // processing order (ADR-042 §3), so the ordering is explicable to an
        // operator watching the job move.
        connection
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cloud_telemetry_bulk_job_projects_queue \
                 ON cloud_telemetry_bulk_job_projects (job_id, status, project_id)",
            )
            .await?;

        // ADR-042 §6/§8: reuse the existing per-project progress surface rather
        // than building a parallel one. Nullable, so every row the CLI writes
        // continues to mean exactly what it meant before.
        connection
            .execute_unprepared(
                "ALTER TABLE cloud_telemetry_backfills \
                 ADD COLUMN IF NOT EXISTS bulk_job_id UUID \
                 REFERENCES cloud_telemetry_bulk_jobs(id) ON DELETE SET NULL",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();

        connection
            .execute_unprepared(
                "ALTER TABLE cloud_telemetry_backfills DROP COLUMN IF EXISTS bulk_job_id",
            )
            .await?;
        connection
            .execute_unprepared("DROP TABLE IF EXISTS cloud_telemetry_bulk_job_projects")
            .await?;
        connection
            .execute_unprepared("DROP TABLE IF EXISTS cloud_telemetry_bulk_jobs")
            .await?;

        Ok(())
    }
}
