// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

/// Last Compose configuration check per project. A row whose `revision` is
/// older than the running binary's validation revision is re-checked once
/// after an upgrade, so a project that the new rules would reject is flagged
/// before its next deployment instead of by it.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE compose_validation_reports (
                    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                    revision INTEGER NOT NULL,
                    passed BOOLEAN NOT NULL,
                    error_class TEXT,
                    error TEXT,
                    checked_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
                )",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE compose_validation_reports")
            .await?;
        Ok(())
    }
}
