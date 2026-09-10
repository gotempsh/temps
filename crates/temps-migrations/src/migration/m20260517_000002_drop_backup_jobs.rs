// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Drop `backup_jobs` + `backup_job_steps` tables and their dependent FKs.
//!
//! The queue-based runner that owned these tables was deleted in the
//! runner→executor migration. The executor writes everything it needs onto
//! the `backups` row directly; engine work is dispatched via the in-process
//! `temps_core::JobQueue` (a tokio broadcast channel today).
//!
//! Dependencies removed:
//! - `backup_schedules.last_job_id` (FK to `backup_jobs.id`) — column dropped
//! - `backup_alerts.job_id` (FK to `backup_jobs.id`) — column dropped;
//!   any open `stalled_job` alerts are auto-resolved first so the column
//!   drop is clean.
//!
//! Down migration is a no-op; recreating the queue schema by hand would be
//! a lot of code for zero benefit (no caller exists to write to it).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Resolve any open 'stalled_job' alerts before yanking the column —
        // otherwise the FK drop would orphan rows that the alert UI would
        // then render with a missing target.
        db.execute_unprepared(
            "UPDATE backup_alerts \
             SET resolved_at = COALESCE(resolved_at, NOW()) \
             WHERE kind = 'stalled_job' AND resolved_at IS NULL",
        )
        .await?;

        // Drop the FK + XOR check + partial unique index that depend on
        // `backup_alerts.job_id`, then the column itself.
        db.execute_unprepared(
            "ALTER TABLE backup_alerts \
             DROP CONSTRAINT IF EXISTS fk_backup_alerts_job_id",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE backup_alerts \
             DROP CONSTRAINT IF EXISTS backup_alerts_target_xor",
        )
        .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS backup_alerts_one_open_per_job")
            .await?;
        db.execute_unprepared("ALTER TABLE backup_alerts DROP COLUMN IF EXISTS job_id")
            .await?;

        // Drop the FK on backup_schedules.last_job_id, then the column.
        db.execute_unprepared(
            "ALTER TABLE backup_schedules \
             DROP CONSTRAINT IF EXISTS fk_backup_schedules_last_job_id",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE backup_schedules DROP COLUMN IF EXISTS last_job_id")
            .await?;

        // Drop the tables. `backup_job_steps` first (it FK's into
        // `backup_jobs`); CASCADE on each as a belt-and-braces guard
        // against any stragglers in dev DBs.
        db.execute_unprepared("DROP TABLE IF EXISTS backup_job_steps CASCADE")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS backup_jobs CASCADE")
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Intentional no-op. The queue-runner code that consumed these
        // tables is gone; recreating empty tables would only confuse
        // operators reading the schema. If a rollback is ever needed,
        // restore from a pre-migration database snapshot.
        Ok(())
    }
}
