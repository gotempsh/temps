// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Server-side inbound filters for error events (ADR 111, D1.4).
//!
//! One row per rule. Ingest never reads this table per event: rules are held
//! in memory and reloaded when they change. `dropped_count` is incremented by
//! a periodic flush of in-memory counters, so the table sees one write per
//! active rule per flush, not one per dropped event.
//!
//! The built-in kinds (`browser_extensions`, `web_crawlers`, `localhost`)
//! exist at most once per project; pattern kinds (`error_message`,
//! `release`) may repeat, one pattern each.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_inbound_filters ( \
                 id SERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 kind TEXT NOT NULL CHECK (kind IN ('browser_extensions', 'web_crawlers', \
                     'localhost', 'error_message', 'release')), \
                 pattern TEXT CHECK (pattern IS NULL OR char_length(pattern) <= 200), \
                 enabled BOOLEAN NOT NULL DEFAULT TRUE, \
                 dropped_count BIGINT NOT NULL DEFAULT 0, \
                 last_dropped_at TIMESTAMPTZ, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 CHECK ((kind IN ('error_message', 'release')) = (pattern IS NOT NULL)))",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_inbound_filters_project \
             ON error_inbound_filters(project_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_error_inbound_filters_builtin \
             ON error_inbound_filters(project_id, kind) \
             WHERE kind IN ('browser_extensions', 'web_crawlers', 'localhost')",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS error_inbound_filters")
            .await?;
        Ok(())
    }
}
