// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Store a visitor with no known environment as `NULL`, not `0`.
//!
//! `visitor.environment_id` was `NOT NULL` with no foreign key, so every
//! writer (the proxy's tracking batch writer, event ingest and session-replay
//! init) encoded "no environment" as a magic `0`. Each reader had to remember
//! that `0` meant absent, and a foreign key to `environments` could never be
//! added while those rows existed.
//!
//! `up` drops `NOT NULL` (a catalog-only change) and rewrites the existing `0`
//! rows to `NULL`. Only rows written without an environment carry `0`, so the
//! update touches that subset, not the whole table.
//!
//! `down` is lossless: `0` and `NULL` meant the same thing, so it maps `NULL`
//! back to `0` and restores `NOT NULL`, the shape an older binary decodes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("ALTER TABLE visitor ALTER COLUMN environment_id DROP NOT NULL")
            .await?;
        db.execute_unprepared("UPDATE visitor SET environment_id = NULL WHERE environment_id = 0")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("UPDATE visitor SET environment_id = 0 WHERE environment_id IS NULL")
            .await?;
        db.execute_unprepared("ALTER TABLE visitor ALTER COLUMN environment_id SET NOT NULL")
            .await?;
        Ok(())
    }
}
