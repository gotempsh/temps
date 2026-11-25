// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Remember the check interval a monitor had before each change.
//!
//! Uptime coverage lets each check result vouch for the endpoint for a grace
//! period derived from the monitor's check interval. Only the current
//! interval was stored, so changing it rewrote the coverage of every window
//! that had already ended: raising 60 s to 3,600 s turned a completed hour
//! with one check from 180 monitored seconds into 3,600, and lowering it did
//! the opposite.
//!
//! Each row records the interval that was in effect up to `changed_at`. The
//! interval a check ran under is the `previous_interval_seconds` of the first
//! change after it, or the monitor's current interval when nothing changed
//! since. Recording changes, rather than a full history seeded per monitor,
//! means existing monitors need no backfill: a monitor without rows has had
//! one interval since this migration ran, and its first change freezes the
//! coverage of everything before it. Coverage computed before this migration
//! already used the current interval, so no existing answer changes.
//!
//! The table is small (one row per interval edit) and read only by one
//! monitor's coverage query, through the `(monitor_id, changed_at)` index.
//! Rows go with their monitor (`ON DELETE CASCADE`).
//!
//! `down` drops the table. An older binary does not read it; what is lost is
//! the record of past intervals, so coverage of earlier windows goes back to
//! following the current interval.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS status_monitor_interval_changes (
                id BIGSERIAL PRIMARY KEY,
                monitor_id INTEGER NOT NULL
                    REFERENCES status_monitors(id) ON DELETE CASCADE,
                previous_interval_seconds INTEGER NOT NULL
                    CHECK (previous_interval_seconds > 0),
                changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_status_monitor_interval_changes_monitor_changed_at
                ON status_monitor_interval_changes (monitor_id, changed_at)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS status_monitor_interval_changes")
            .await?;
        Ok(())
    }
}
