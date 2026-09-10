// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Guard `lifecycle_reconcile_failed_at` against out-of-order concurrent
//! writes.
//!
//! Two S3 lifecycle reconciles for the same source can overlap — an
//! event-driven reconcile from an earlier schedule mutation and the hourly
//! sweep, or two schedule mutations in quick succession — and finish out of
//! start order. Without a way to detect that, whichever `UPDATE` lands last
//! in wall-clock time wins even if it started first and saw stale state: an
//! older attempt's success could clear a newer attempt's failure marker,
//! silently dropping the source from every future retry.
//!
//! `lifecycle_reconcile_generation` is a monotonic counter bumped
//! atomically at the start of every reconcile attempt
//! (`S3LifecycleService::begin_reconcile_attempt`). The attempt's outcome is
//! only written if the counter still matches what it read at the start
//! (`record_reconcile_attempt`), so only the most-recently-*started*
//! attempt's result can ever be persisted — an older attempt that finishes
//! late has its write silently dropped instead of clobbering a newer one.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE s3_sources \
             ADD COLUMN IF NOT EXISTS lifecycle_reconcile_generation INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE s3_sources DROP COLUMN IF EXISTS lifecycle_reconcile_generation",
        )
        .await?;
        Ok(())
    }
}
