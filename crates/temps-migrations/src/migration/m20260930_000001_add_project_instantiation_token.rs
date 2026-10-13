// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to add `instantiation_token` to the `projects` table.
//!
//! A template instantiation resumes after a crash, and the step that creates
//! a service may have inserted its project without recording the id. The
//! token names the instantiation and template service the project was
//! created for, so a resumed run adopts a project only when it carries its
//! own token. The row is inserted with a `pending:` spelling and takes the
//! final token once its environment, variables and links exist, so the final
//! token also proves the project is complete. Nothing else about a project
//! (name, slug, creator, timestamps) identifies the run that made it.
//!
//! The column is NULL for every project that did not come from an
//! instantiation. The partial unique index means at most one project can ever
//! carry a given token, so a retry cannot create a second service for the
//! same step.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Nullable with no default: a metadata-only change, no table rewrite.
        db.execute_unprepared(
            "ALTER TABLE projects ADD COLUMN IF NOT EXISTS instantiation_token TEXT",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_instantiation_token \
             ON projects (instantiation_token) \
             WHERE instantiation_token IS NOT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_projects_instantiation_token")
            .await?;
        db.execute_unprepared("ALTER TABLE projects DROP COLUMN IF EXISTS instantiation_token")
            .await?;
        Ok(())
    }
}
