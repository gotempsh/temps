// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which store the Global Logs line index lives in (ADR-047 §8).
//!
//! The index can be in the instance's ClickHouse, Temps Cloud's ClickHouse
//! or the control-plane TimescaleDB, chosen at startup. `log_chunks.indexed_at`
//! only says *that* a chunk was indexed, not *where*; when the store changes
//! the plugin compares against this row, clears `indexed_at` and lets the
//! reindexer rebuild the index where queries now look. One row, id = 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS log_line_index_state (\
                    id SMALLINT PRIMARY KEY CHECK (id = 1), \
                    backend VARCHAR NOT NULL, \
                    changed_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS log_line_index_state")
            .await?;
        Ok(())
    }
}
