// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stamp `backups.expires_at` on rows created before the deadline was
//! recorded at creation time. A schedule-owned backup is retained until
//! `started_at` plus its schedule's retention period, which is exactly the
//! rule the retention sweep applies; without this the console would read
//! "Kept until deleted" for every existing backup until its schedule's
//! retention is next edited. Schedule-less backups stay `NULL`: nothing
//! deletes them on a timer.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const UP_SQL: &str = "UPDATE backups b
     SET expires_at = b.started_at + (s.retention_period * interval '1 day')
     FROM backup_schedules s
     WHERE b.schedule_id = s.id
       AND b.expires_at IS NULL
       AND s.retention_period >= 1";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The value is derived, so clearing it loses nothing the schedule
        // does not still hold; the previous code never read it.
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE backups SET expires_at = NULL WHERE schedule_id IS NOT NULL",
            )
            .await?;
        Ok(())
    }
}
