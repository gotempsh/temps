// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-047 §4/§6: `log_chunks.indexed_at` — when a chunk's lines were
//! accepted by the ClickHouse line index. `NULL` means "not indexed" and is
//! the reindexer's work queue: chunks sealed while the index was down, or
//! before ClickHouse was configured at all. A partial index keeps that
//! queue scan cheap however many chunks exist.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE log_chunks ADD COLUMN IF NOT EXISTS indexed_at TIMESTAMPTZ",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_log_chunks_unindexed ON log_chunks (seq DESC) \
             WHERE deleted_at IS NULL AND indexed_at IS NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_log_chunks_unindexed")
            .await?;
        db.execute_unprepared("ALTER TABLE log_chunks DROP COLUMN IF EXISTS indexed_at")
            .await?;
        Ok(())
    }
}
