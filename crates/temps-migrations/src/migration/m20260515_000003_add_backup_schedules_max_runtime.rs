// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration that adds `max_runtime_secs` to `backup_schedules`.
//!
//! Nullable on purpose — `NULL` means "use engine default." When the scheduler
//! fires `enqueue_scheduled_backup` it reads this column and, if set, passes
//! it down as `EnqueueJobParams::max_runtime_secs` so the job row gets a
//! per-schedule timeout rather than the engine's conservative global default.
//!
//! The UI form for schedule creation / editing can expose this as an optional
//! number field; the API already surfaces it in `BackupScheduleResponse`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE backup_schedules
    ADD COLUMN IF NOT EXISTS max_runtime_secs BIGINT;
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
ALTER TABLE backup_schedules DROP COLUMN IF EXISTS max_runtime_secs;
            "#,
        )
        .await?;

        Ok(())
    }
}
