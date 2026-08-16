//! Migration that creates the `backup_jobs` and `backup_job_steps` tables,
//! and amends `backup_schedules` with a `last_job_id` column.
//!
//! These tables are the execution queue for ADR-014 (Unified Backup Execution
//! Architecture). `backup_jobs` is the claim-based queue; `backup_job_steps`
//! is an append-only audit of every step transition. Both tables are purely
//! additive — no existing columns are touched.
//!
//! See `docs/adr/014-unified-backup-architecture.md` for the full schema
//! rationale, partial index description, and claim-query semantics.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // ── backup_jobs ───────────────────────────────────────────────────────
        // One row per execution attempt (including retries). The runner claims
        // rows atomically via FOR UPDATE SKIP LOCKED and writes final state back
        // to the parent `backups` row on Done or terminal failure.

        manager
            .create_table(
                Table::create()
                    .table(BackupJobs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BackupJobs::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // FK to the parent `backups` row. Cascade-delete keeps the
                    // jobs table clean when a backup record is pruned by retention.
                    .col(ColumnDef::new(BackupJobs::BackupId).integer().not_null())
                    // Identifies the engine implementation. Must match the
                    // value returned by `BackupEngine::engine()`.
                    // Examples: 'postgres_walg', 'postgres_pgdump', 'redis', etc.
                    .col(ColumnDef::new(BackupJobs::Engine).text().not_null())
                    // 'control_plane' | 'external_service'
                    .col(ColumnDef::new(BackupJobs::TargetKind).text().not_null())
                    // NULL for control_plane backups; FK to external_services otherwise.
                    .col(ColumnDef::new(BackupJobs::TargetId).integer().null())
                    // Engine-specific parameters (e.g., S3 bucket, compression
                    // settings, max_concurrent override). Passed verbatim to
                    // `BackupEngine::execute`.
                    .col(
                        ColumnDef::new(BackupJobs::Params)
                            .json_binary()
                            .not_null()
                            .default("{}"),
                    )
                    // Lifecycle state. CHECK constraint enforced via raw SQL
                    // below (sea-orm migration DSL has no CHECK support).
                    .col(
                        ColumnDef::new(BackupJobs::State)
                            .text()
                            .not_null()
                            .default("pending"),
                    )
                    // Name of the last completed step. NULL on first attempt.
                    // On a resume, the engine receives this value in StepCursor
                    // and must skip to the next step.
                    .col(ColumnDef::new(BackupJobs::Step).text().null())
                    // Durable cursor the engine wrote at the last completed step.
                    // Passed back verbatim on resume so the engine can reconstruct
                    // its position (e.g., last uploaded key for S3 mirror sync).
                    .col(
                        ColumnDef::new(BackupJobs::StepState)
                            .json_binary()
                            .not_null()
                            .default("{}"),
                    )
                    // Total number of times this job has been claimed and run.
                    // Incremented atomically by the claim query.
                    .col(
                        ColumnDef::new(BackupJobs::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    // Maximum number of attempts before the job is permanently
                    // failed. Schedulers may override per engine via `params`.
                    .col(
                        ColumnDef::new(BackupJobs::MaxAttempts)
                            .integer()
                            .not_null()
                            .default(3),
                    )
                    // Rotated on every claim. The runner uses this as a fencing
                    // token: UPDATE ... WHERE claim_token = $N prevents a stale
                    // runner from overwriting a newer owner's progress.
                    .col(ColumnDef::new(BackupJobs::ClaimToken).uuid().null())
                    // Hostname or instance-id of the process that currently holds
                    // this job. Set on claim, cleared on completion/failure.
                    .col(ColumnDef::new(BackupJobs::ClaimedBy).text().null())
                    // Hard expiry of the current lease. The runner must either
                    // complete a step (which extends the lease) or emit a Heartbeat
                    // before this timestamp expires, or a competing runner will
                    // reclaim the job.
                    .col(
                        ColumnDef::new(BackupJobs::LeasedUntil)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    // Earliest time the job may be claimed again. Set to NOW() on
                    // insert; advanced by the backoff formula on retry.
                    .col(
                        ColumnDef::new(BackupJobs::NextAttemptAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(BackupJobs::ErrorMessage).text().null())
                    // Stamped on first claim; not reset on retry.
                    .col(
                        ColumnDef::new(BackupJobs::StartedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    // Stamped by the runner at the exact moment of Done or
                    // terminal failure. Never fabricated at boot time.
                    .col(
                        ColumnDef::new(BackupJobs::FinishedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(BackupJobs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(BackupJobs::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backup_jobs_backup_id")
                            .from(BackupJobs::Table, BackupJobs::BackupId)
                            .to(Backups::Table, Backups::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // CHECK constraint on state: sea-orm migration DSL does not support
        // CHECK constraints, so we use raw SQL — same pattern as
        // `m20260427_000002_add_dns_service_endpoints.rs`.
        db.execute_unprepared(
            "ALTER TABLE backup_jobs \
             ADD CONSTRAINT backup_jobs_state_valid \
             CHECK (state IN ('pending','running','completed','failed','cancelled'))",
        )
        .await?;

        // Primary polling index: the claim query filters on
        // `state = 'pending' AND next_attempt_at <= NOW()`. The partial WHERE
        // clause reduces index size dramatically — completed/failed rows are
        // never scanned by the poller.
        // Note: sea-orm migration DSL has no partial-index support; raw SQL is
        // the established pattern in this codebase (see m20260328, m20260427).
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS backup_jobs_claimable_idx \
             ON backup_jobs (next_attempt_at) \
             WHERE state = 'pending'",
        )
        .await?;

        // Secondary index for parent-row lookups (UI, retention queries).
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS backup_jobs_backup_id_idx \
             ON backup_jobs (backup_id)",
        )
        .await?;

        // ── backup_job_steps ─────────────────────────────────────────────────
        // Append-only audit of every step transition, including resume events.
        // Written inside a transaction by `persist_step_completed` with the
        // claim_token fencing check on the parent backup_jobs row.

        manager
            .create_table(
                Table::create()
                    .table(BackupJobSteps::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BackupJobSteps::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(BackupJobSteps::JobId)
                            .big_integer()
                            .not_null(),
                    )
                    // Which attempt of the parent job this step belongs to.
                    // Allows the UI to show per-attempt step timelines.
                    .col(ColumnDef::new(BackupJobSteps::Attempt).integer().not_null())
                    .col(ColumnDef::new(BackupJobSteps::Step).text().not_null())
                    // 'started' | 'completed' | 'failed' | 'resumed'
                    .col(ColumnDef::new(BackupJobSteps::State).text().not_null())
                    // Durable cursor at this step — the value the engine will
                    // receive as StepCursor.durable_state on the next resume.
                    .col(
                        ColumnDef::new(BackupJobSteps::DurableState)
                            .json_binary()
                            .not_null()
                            .default("{}"),
                    )
                    // Human-readable progress note from the engine, if any.
                    .col(ColumnDef::new(BackupJobSteps::Message).text().null())
                    .col(
                        ColumnDef::new(BackupJobSteps::OccurredAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backup_job_steps_job_id")
                            .from(BackupJobSteps::Table, BackupJobSteps::JobId)
                            .to(BackupJobs::Table, BackupJobs::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // CHECK constraint on step state.
        db.execute_unprepared(
            "ALTER TABLE backup_job_steps \
             ADD CONSTRAINT backup_job_steps_state_valid \
             CHECK (state IN ('started','completed','failed','resumed'))",
        )
        .await?;

        // Index for the primary query pattern: list all steps for a job,
        // ordered by occurrence time (UI progress timeline + resume cursor).
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS backup_job_steps_job_id_idx \
             ON backup_job_steps (job_id, occurred_at)",
        )
        .await?;

        // ── backup_schedules amendment ────────────────────────────────────────
        // Tracks the most recently enqueued backup_jobs row for each schedule,
        // enabling the UI to show "queued but not yet started" separately from
        // "never ran". ON DELETE SET NULL so pruning old jobs doesn't break the
        // schedule row.
        manager
            .alter_table(
                Table::alter()
                    .table(BackupSchedules::Table)
                    .add_column(
                        ColumnDef::new(BackupSchedules::LastJobId)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        db.execute_unprepared(
            "ALTER TABLE backup_schedules \
             ADD CONSTRAINT fk_backup_schedules_last_job_id \
             FOREIGN KEY (last_job_id) REFERENCES backup_jobs(id) ON DELETE SET NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Everything here is IF EXISTS-tolerant: m20260517_000002 drops
        // backup_jobs/backup_job_steps (and the last_job_id column) with a
        // no-op down(), so a full rollback reaches this migration with
        // those objects already gone.
        db.execute_unprepared(
            "ALTER TABLE IF EXISTS backup_schedules \
             DROP CONSTRAINT IF EXISTS fk_backup_schedules_last_job_id",
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE IF EXISTS backup_schedules DROP COLUMN IF EXISTS last_job_id",
        )
        .await?;

        // Drop backup_job_steps before backup_jobs (FK dependency).
        manager
            .drop_table(
                Table::drop()
                    .table(BackupJobSteps::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(BackupJobs::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum BackupJobs {
    Table,
    Id,
    BackupId,
    Engine,
    TargetKind,
    TargetId,
    Params,
    State,
    Step,
    StepState,
    Attempts,
    MaxAttempts,
    ClaimToken,
    ClaimedBy,
    LeasedUntil,
    NextAttemptAt,
    ErrorMessage,
    StartedAt,
    FinishedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum BackupJobSteps {
    Table,
    Id,
    JobId,
    Attempt,
    Step,
    State,
    DurableState,
    Message,
    OccurredAt,
}

#[derive(DeriveIden)]
enum BackupSchedules {
    Table,
    LastJobId,
}

#[derive(DeriveIden)]
enum Backups {
    Table,
    Id,
}
