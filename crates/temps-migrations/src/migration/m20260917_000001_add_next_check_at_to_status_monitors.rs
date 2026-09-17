// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Make the status-monitor scheduler honour each monitor's configured interval.
//!
//! `status_monitors.check_interval_seconds` has existed since the initial
//! schema but nothing ever read it: the scheduler woke up every 60 seconds and
//! probed *every* active monitor, so a monitor configured for a 10-minute
//! interval was still hit once a minute and a monitor configured for 30
//! seconds still only got checked once a minute. On an instance with a few
//! hundred environments that is a fixed per-minute burst of outbound HTTP
//! regardless of what the operator asked for.
//!
//! `next_check_at` turns the sweep into a due-list: the scheduler selects only
//! the monitors whose turn it is and stamps the next due time after each
//! probe. NULL means "never scheduled yet" and is treated as due, so existing
//! rows are picked up on the first cycle after this migration without a
//! backfill. The partial-free composite index matches the sweep predicate
//! (`is_active = true AND (next_check_at IS NULL OR next_check_at <= now())`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE status_monitors \
             ADD COLUMN IF NOT EXISTS next_check_at TIMESTAMPTZ NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_status_monitors_due \
             ON status_monitors (is_active, next_check_at)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_status_monitors_due")
            .await?;
        db.execute_unprepared("ALTER TABLE status_monitors DROP COLUMN IF EXISTS next_check_at")
            .await?;
        Ok(())
    }
}
