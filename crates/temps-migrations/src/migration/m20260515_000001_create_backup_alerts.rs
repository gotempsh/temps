// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration that creates the `backup_alerts` table.
//!
//! Backup alerts track two failure modes that are otherwise invisible:
//!
//! 1. **Overdue schedules**: `backup_schedules` that have `enabled=true` and
//!    `next_run < NOW() - 1h`. The scheduler tick did not enqueue a job when
//!    it should have (scheduler dead or wedged).
//!
//! 2. **Stalled jobs**: `backup_jobs` that are `state='pending'` and were
//!    created more than 1 hour ago. The runner never claimed the job (runner
//!    dead or wedged).
//!
//! The watcher service (`temps-backup/src/services/alerts.rs`) opens and
//! auto-resolves rows as conditions change. The UI reads open alerts on every
//! Backups page load and displays a banner.
//!
//! Partial unique indexes prevent duplicate open alerts per target so the
//! INSERT ... ON CONFLICT DO NOTHING pattern in the watcher is safe.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // ── backup_alerts ─────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(BackupAlerts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BackupAlerts::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // 'overdue_schedule' | 'stalled_job'
                    .col(ColumnDef::new(BackupAlerts::Kind).text().not_null())
                    // FK to backup_schedules — set for 'overdue_schedule' alerts.
                    .col(ColumnDef::new(BackupAlerts::ScheduleId).integer().null())
                    // FK to backup_jobs — set for 'stalled_job' alerts.
                    .col(ColumnDef::new(BackupAlerts::JobId).big_integer().null())
                    // 'warning' | 'critical'
                    .col(
                        ColumnDef::new(BackupAlerts::Severity)
                            .text()
                            .not_null()
                            .default("warning"),
                    )
                    // Human-readable description for the UI banner.
                    .col(ColumnDef::new(BackupAlerts::Message).text().not_null())
                    .col(
                        ColumnDef::new(BackupAlerts::OpenedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    // NULL while open. Set to NOW() by the watcher when the
                    // condition clears (schedule fires or job is claimed).
                    .col(
                        ColumnDef::new(BackupAlerts::ResolvedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backup_alerts_schedule_id")
                            .from(BackupAlerts::Table, BackupAlerts::ScheduleId)
                            .to(BackupSchedules::Table, BackupSchedules::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_backup_alerts_job_id")
                            .from(BackupAlerts::Table, BackupAlerts::JobId)
                            .to(BackupJobs::Table, BackupJobs::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // CHECK constraint: kind must be one of the two known values.
        db.execute_unprepared(
            "ALTER TABLE backup_alerts \
             ADD CONSTRAINT backup_alerts_kind_valid \
             CHECK (kind IN ('overdue_schedule', 'stalled_job'))",
        )
        .await?;

        // CHECK constraint: severity must be 'warning' or 'critical'.
        db.execute_unprepared(
            "ALTER TABLE backup_alerts \
             ADD CONSTRAINT backup_alerts_severity_valid \
             CHECK (severity IN ('warning', 'critical'))",
        )
        .await?;

        // XOR constraint: exactly one of schedule_id / job_id must be set.
        db.execute_unprepared(
            "ALTER TABLE backup_alerts \
             ADD CONSTRAINT backup_alerts_target_xor \
             CHECK ( \
               (schedule_id IS NOT NULL AND job_id IS NULL) \
               OR (schedule_id IS NULL AND job_id IS NOT NULL) \
             )",
        )
        .await?;

        // Partial unique index: at most one open alert per schedule.
        // ON CONFLICT DO NOTHING in the watcher INSERT relies on this.
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS backup_alerts_one_open_per_schedule \
             ON backup_alerts (schedule_id) \
             WHERE resolved_at IS NULL AND schedule_id IS NOT NULL",
        )
        .await?;

        // Partial unique index: at most one open alert per job.
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS backup_alerts_one_open_per_job \
             ON backup_alerts (job_id) \
             WHERE resolved_at IS NULL AND job_id IS NOT NULL",
        )
        .await?;

        // Index for listing open alerts in reverse-chronological order.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS backup_alerts_open_idx \
             ON backup_alerts (opened_at DESC) \
             WHERE resolved_at IS NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BackupAlerts::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum BackupAlerts {
    Table,
    Id,
    Kind,
    ScheduleId,
    JobId,
    Severity,
    Message,
    OpenedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
enum BackupSchedules {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum BackupJobs {
    Table,
    Id,
}
