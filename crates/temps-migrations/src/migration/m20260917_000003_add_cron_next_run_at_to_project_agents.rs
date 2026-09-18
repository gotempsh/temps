// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Give scheduled agents a due time so the agent cron scheduler stops
//! full-scanning `project_agents` once a minute.
//!
//! The scheduler loaded *every* enabled agent each minute, decrypted each
//! row's provider credentials, and then discarded all but the handful with a
//! `schedule.cron` entry in `trigger_config` — 1,440 full scans and 1,440
//! rounds of needless decryption per day on an instance where nothing is
//! scheduled at all.
//!
//! `cron_next_run_at` lets the scheduler ask for just the agents whose turn it
//! is. NULL means "not computed yet" and is treated as due, so existing rows
//! need no backfill and a schedule edit can simply reset the column to NULL to
//! have it recomputed on the next tick. The index matches the scheduler's
//! predicate, including the JSONB test that excludes agents with no cron
//! schedule at all.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE project_agents \
             ADD COLUMN IF NOT EXISTS cron_next_run_at TIMESTAMPTZ NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_project_agents_cron_due \
             ON project_agents (enabled, cron_next_run_at) \
             WHERE (trigger_config -> 'schedule' ->> 'cron') IS NOT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_project_agents_cron_due")
            .await?;
        db.execute_unprepared("ALTER TABLE project_agents DROP COLUMN IF EXISTS cron_next_run_at")
            .await?;
        Ok(())
    }
}
