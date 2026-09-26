// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-container collector high-water mark, independent of chunk rows.
//!
//! The collector resumes a container's Docker log stream from the newest
//! stored chunk's `ended_at`. That position used to live only in
//! `log_chunks`, so once retention or a purge tombstoned every chunk of a
//! container and GC hard-deleted the rows, a restart replayed the
//! container's entire log from Docker — resurrecting exactly the lines a
//! purge had removed. This table is upserted on every seal and never
//! garbage-collected with the chunks.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS log_collector_positions (\
                container_id VARCHAR PRIMARY KEY, \
                last_ts TIMESTAMPTZ NOT NULL, \
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        )
        .await?;
        // Seed from what is currently known so existing installs do not
        // lose their position on the first restart after this migration.
        db.execute_unprepared(
            "INSERT INTO log_collector_positions (container_id, last_ts) \
             SELECT container_id, MAX(ended_at) FROM log_chunks GROUP BY container_id \
             ON CONFLICT (container_id) DO NOTHING",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS log_collector_positions")
            .await?;
        Ok(())
    }
}
