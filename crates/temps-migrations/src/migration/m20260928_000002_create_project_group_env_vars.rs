// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Environment variables set on a project group and inherited by every
//! project in it. A project's own variable with the same key always wins.
//!
//! Scope is matched by environment slug, which is how environments line up
//! across projects (`production`, `staging`, …): NULL means every
//! non-preview environment, plus previews when `include_in_preview` is set.
//! Values are always encrypted at rest.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS project_group_env_vars (\
                 id SERIAL PRIMARY KEY, \
                 group_id INTEGER NOT NULL \
                     REFERENCES project_groups(id) ON DELETE CASCADE, \
                 key VARCHAR(255) NOT NULL, \
                 value TEXT NOT NULL, \
                 is_secret BOOLEAN NOT NULL DEFAULT FALSE, \
                 environment_slug VARCHAR(100), \
                 include_in_preview BOOLEAN NOT NULL DEFAULT FALSE, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\
             )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_project_group_env_vars_scope \
             ON project_group_env_vars (group_id, key, COALESCE(environment_slug, ''))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS project_group_env_vars")
            .await?;
        Ok(())
    }
}
