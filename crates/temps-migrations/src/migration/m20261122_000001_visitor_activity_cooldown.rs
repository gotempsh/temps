// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! When a project's visitor activity report may call the AI provider again.
//!
//! Report runs used to be throttled from `last_started_at`, so a run that
//! failed before reaching the provider still blocked retries for five minutes.
//! The cooldown is now set only after a run spends provider tokens (or the
//! provider rate-limits it). NULL, the value for existing rows, means no
//! cooldown is active. No backfill: the old throttle simply stops applying.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE visitor_activity_reports
                 ADD COLUMN IF NOT EXISTS cooldown_until TIMESTAMPTZ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE visitor_activity_reports DROP COLUMN IF EXISTS cooldown_until",
            )
            .await?;
        Ok(())
    }
}
