// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-link variable aliases for a project's linked service.
//!
//! A linked database injects its own names (`POSTGRES_URL`); most apps ask for
//! another (`DATABASE_URL`). `env_aliases` maps the app's name onto the
//! injected one (`{"DATABASE_URL": "POSTGRES_URL"}`), so the link injects
//! both. Existing links get `{}`: they inject exactly what they did before,
//! and an older binary that does not read the column is unaffected.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_services \
                 ADD COLUMN IF NOT EXISTS env_aliases jsonb NOT NULL DEFAULT '{}'::jsonb",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the column loses only aliases; the injected names remain.
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE project_services DROP COLUMN IF EXISTS env_aliases")
            .await?;
        Ok(())
    }
}
