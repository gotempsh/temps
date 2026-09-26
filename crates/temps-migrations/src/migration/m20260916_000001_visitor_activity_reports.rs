// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE visitor_activity_reports (
                project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                settings JSONB NOT NULL,
                revision INTEGER NOT NULL DEFAULT 1,
                daily_enabled BOOLEAN NOT NULL DEFAULT FALSE,
                next_run_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                locked_until TIMESTAMPTZ,
                last_started_at TIMESTAMPTZ,
                last_error TEXT,
                report JSONB
            );
            CREATE INDEX visitor_activity_reports_due_idx ON visitor_activity_reports(next_run_at)
                WHERE daily_enabled = TRUE;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE visitor_activity_reports")
            .await?;
        Ok(())
    }
}
