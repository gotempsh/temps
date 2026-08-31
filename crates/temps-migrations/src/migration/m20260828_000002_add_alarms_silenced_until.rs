// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add `alarms.silenced_until` — lets an operator mute an alert (and its
//! future re-fires of the same type/scope) for a chosen duration, without
//! permanently resolving it. Separate from `acknowledged_at`/`resolved_at`:
//! those are permanent lifecycle transitions, while a silence expires on its
//! own and the alarm goes back to notifying normally.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "ALTER TABLE alarms ADD COLUMN IF NOT EXISTS silenced_until TIMESTAMPTZ NULL",
        )
        .await?;

        // Speeds up the cooldown/re-fire check, which looks for a still-silenced
        // row matching (project_id, alarm_type, deployment_id, container_id,
        // service_id) — filtering on `silenced_until IS NOT NULL` first is
        // selective since silenced rows are a small minority.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_alarms_silenced_until \
             ON alarms (silenced_until) WHERE silenced_until IS NOT NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("DROP INDEX IF EXISTS idx_alarms_silenced_until")
            .await?;
        db.execute_unprepared("ALTER TABLE alarms DROP COLUMN IF EXISTS silenced_until")
            .await?;

        Ok(())
    }
}
