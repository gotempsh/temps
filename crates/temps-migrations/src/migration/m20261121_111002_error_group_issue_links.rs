// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Issues linked to error groups (ADR 111, D5).
//!
//! A link is either an issue Temps opened in the project's GitHub or GitLab
//! repository (`provider` `github` or `gitlab`, with its `repository` and
//! `number`, whose `state` Temps reads back on request), or any other URL a
//! person pasted (`provider` `link`: Jira, Linear, an issue elsewhere), which
//! is only stored. A URL is linked to a group once.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_group_issue_links ( \
                 id BIGSERIAL PRIMARY KEY, \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 error_group_id INTEGER NOT NULL REFERENCES error_groups(id) ON DELETE CASCADE, \
                 provider TEXT NOT NULL CHECK (provider IN ('github', 'gitlab', 'link')), \
                 repository TEXT, \
                 number BIGINT, \
                 url TEXT NOT NULL, \
                 title TEXT NOT NULL, \
                 state TEXT CHECK (state IN ('open', 'closed')), \
                 state_checked_at TIMESTAMPTZ, \
                 created_by INTEGER REFERENCES users(id) ON DELETE SET NULL, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 UNIQUE (error_group_id, url), \
                 CHECK ((provider = 'link') = (number IS NULL)))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS error_group_issue_links")
            .await?;
        Ok(())
    }
}
