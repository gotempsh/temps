// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Index `cron_executions` by cron and execution time.
//!
//! Every read of this table filters on `cron_id` and orders by `executed_at`:
//! the per-cron execution history, and the last-successful-run lookup that
//! a failed invocation's alarm includes. Without an index each of those
//! scans every execution of every cron.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_cron_executions_cron_id_executed_at \
                 ON cron_executions (cron_id, executed_at DESC)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_cron_executions_cron_id_executed_at")
            .await?;
        Ok(())
    }
}
