// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved log searches (ADR 108 D1).
//!
//! A saved search is a named, per-project log query: the same filter set the
//! global log search accepts (source, levels, environments, services,
//! containers, nodes, deployment, free text and attribute predicates) plus a
//! relative time range for the console. It is the unit a log alert and a
//! dashboard tile reference, so the query is stored once and evaluated by
//! id rather than copied into every consumer.
//!
//! The query is `jsonb` because it mirrors a wire type
//! (`GlobalLogSearchRequest` minus the window and cursor) whose fields are
//! validated by the service on every write; the service and DTO layers are
//! fully typed. Names are unique per project (case-sensitive, trimmed by the
//! service). Deleting a project removes its searches; deleting the creating
//! user keeps the search and clears `created_by`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            CREATE TABLE IF NOT EXISTS log_saved_searches (
                id SERIAL PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                query JSONB NOT NULL DEFAULT '{}'::jsonb,
                time_range TEXT NOT NULL DEFAULT '1h',
                pinned BOOLEAN NOT NULL DEFAULT FALSE,
                created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_log_saved_searches_project_name \
             ON log_saved_searches(project_id, name)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS log_saved_searches")
            .await?;
        Ok(())
    }
}
