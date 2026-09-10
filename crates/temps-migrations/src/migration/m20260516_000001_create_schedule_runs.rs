// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration: create `schedule_runs` table and add `schedule_run_id` to `backups`.
//!
//! ## Purpose
//!
//! ADR-014 Phase 3 fan-out: each scheduler tick (or "Run now" click) fans out
//! to multiple backup jobs — one control-plane + one per supported external
//! service. Before this migration, the run-history page showed one row per
//! backup job, making it impossible to correlate the jobs from a single tick.
//!
//! `schedule_runs` is the parent row: one per tick / one per manual trigger.
//! Every `backups` row created by that fan-out is linked via `schedule_run_id`.
//!
//! ## Backward compatibility
//!
//! Existing `backups` rows keep `schedule_run_id = NULL`. The list query
//! surfaces them as synthetic single-job runs via the legacy `schedule_id`
//! linkage so old history does not disappear from the UI.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // ── schedule_runs ─────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(ScheduleRuns::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ScheduleRuns::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ScheduleRuns::ScheduleId)
                            .integer()
                            .not_null(),
                    )
                    // 'cron' | 'manual'
                    .col(ColumnDef::new(ScheduleRuns::TriggeredBy).text().not_null())
                    // NULL for cron-triggered runs.
                    .col(
                        ColumnDef::new(ScheduleRuns::TriggeredByUserId)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ScheduleRuns::StartedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    // NULL while any child backup is still pending or running.
                    // Set by `mark_schedule_run_finished_if_done` when the last
                    // child reaches a terminal state.
                    .col(
                        ColumnDef::new(ScheduleRuns::FinishedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ScheduleRuns::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_schedule_runs_schedule_id")
                            .from(ScheduleRuns::Table, ScheduleRuns::ScheduleId)
                            .to(BackupSchedules::Table, BackupSchedules::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_schedule_runs_triggered_by_user_id")
                            .from(ScheduleRuns::Table, ScheduleRuns::TriggeredByUserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // CHECK constraint: triggered_by must be 'cron' or 'manual'.
        db.execute_unprepared(
            "ALTER TABLE schedule_runs \
             ADD CONSTRAINT schedule_runs_triggered_by_valid \
             CHECK (triggered_by IN ('cron', 'manual'))",
        )
        .await?;

        // Primary lookup index: list runs for a schedule ordered newest-first.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS schedule_runs_schedule_id_started_at_idx \
             ON schedule_runs (schedule_id, started_at DESC)",
        )
        .await?;

        // ── backups.schedule_run_id ───────────────────────────────────────────
        db.execute_unprepared(
            "ALTER TABLE backups \
             ADD COLUMN IF NOT EXISTS schedule_run_id BIGINT \
             REFERENCES schedule_runs(id) ON DELETE SET NULL",
        )
        .await?;

        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS backups_schedule_run_id_idx \
             ON backups (schedule_run_id) \
             WHERE schedule_run_id IS NOT NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop the FK column from backups first to avoid constraint violations.
        db.execute_unprepared("ALTER TABLE backups DROP COLUMN IF EXISTS schedule_run_id")
            .await?;

        manager
            .drop_table(Table::drop().table(ScheduleRuns::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum ScheduleRuns {
    Table,
    Id,
    ScheduleId,
    TriggeredBy,
    TriggeredByUserId,
    StartedAt,
    FinishedAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum BackupSchedules {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
