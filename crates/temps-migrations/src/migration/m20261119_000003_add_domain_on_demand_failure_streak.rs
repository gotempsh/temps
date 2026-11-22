// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep an on-demand certificate's failure streak on its `domains` row.
//!
//! A hostname whose on-demand certificate keeps failing backs off on a ladder
//! (5m, 15m, 1h, 4h, 24h). The rung is the number of failures in a row, so it
//! must be stored with the backoff deadline it produces: one row update writes
//! both, and nothing else (a lost audit row, a failed history read) can lower
//! the rung. `on_demand_last_failure_at` lets a host that has been quiet for
//! two days start the ladder over.
//!
//! The streak is `NOT NULL DEFAULT 0`, which PostgreSQL adds without
//! rewriting the table; the timestamp is nullable. Existing rows start at the
//! bottom of the ladder, which is where every failure started before. The
//! previous binary ignores both columns (and inserts get the default), so a
//! rollback keeps working. No backfill.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE domains \
                 ADD COLUMN IF NOT EXISTS on_demand_failure_streak integer NOT NULL DEFAULT 0, \
                 ADD COLUMN IF NOT EXISTS on_demand_last_failure_at timestamptz",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE domains \
                 DROP COLUMN IF EXISTS on_demand_last_failure_at, \
                 DROP COLUMN IF EXISTS on_demand_failure_streak",
            )
            .await?;
        Ok(())
    }
}
