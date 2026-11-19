// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keys for the HTTP log ingest endpoint (ADR 073 D1).
//!
//! `POST /logs/ingest` lets a host, a collector (Vector, Fluent Bit) or an
//! application outside Temps send newline-delimited JSON log lines into a
//! project. It follows the analytics ingest-key model (ADR 040): a
//! per-project row, optionally pinned to one environment, soft-revoked,
//! with usage counters. Unlike an analytics key, which ships in browser
//! JavaScript and is public by design, a log ingest key sits in a server-side
//! collector and accepts arbitrary log text into the project's store, so it
//! is a secret: only its SHA-256 is stored (`key_hash`), the plaintext is
//! returned once at creation, and `key_prefix` keeps the first characters
//! for display. Rate limiting is not a column: the shared per-project log
//! ingestion budget (`container_logs.ingest_*` and the per-project overrides)
//! applies to every key exactly as it does to a container.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS log_ingest_keys (
                id SERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                key_prefix TEXT NOT NULL,
                key_hash TEXT NOT NULL,
                default_service TEXT NOT NULL DEFAULT 'external',
                is_active BOOLEAN NOT NULL DEFAULT TRUE,
                revoked_at TIMESTAMPTZ,
                line_count BIGINT NOT NULL DEFAULT 0,
                last_used_at TIMESTAMPTZ,
                created_by_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_log_ingest_keys_hash ON log_ingest_keys(key_hash)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_log_ingest_keys_project ON log_ingest_keys(project_id)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS log_ingest_keys")
            .await?;
        Ok(())
    }
}
