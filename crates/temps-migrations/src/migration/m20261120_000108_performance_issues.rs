// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Performance issues as error groups (ADR 111, D2).
//!
//! `error_groups.issue_type` separates groups filed from an exception
//! (`error`, every existing row) from groups a trace detector filed
//! (`performance`): an N+1 query, a slow database query, consecutive HTTP
//! calls, an oversized payload. Both share the workflow, alerts and inbox.
//!
//! `performance_scan_state` holds, per project, where the detector job
//! stopped and what it found, so the job resumes after a restart and the
//! console can say when traces were last checked.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE error_groups \
             ADD COLUMN IF NOT EXISTS issue_type TEXT NOT NULL DEFAULT 'error'",
        )
        .await?;
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'error_groups_issue_type') THEN \
                 ALTER TABLE error_groups ADD CONSTRAINT error_groups_issue_type \
                   CHECK (issue_type IN ('error', 'performance')); \
               END IF; \
             END $$",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_groups_performance \
             ON error_groups (project_id, last_seen DESC) WHERE issue_type = 'performance'",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS performance_scan_state ( \
                 project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE, \
                 scanned_until TIMESTAMPTZ NOT NULL, \
                 last_run_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 traces_scanned BIGINT NOT NULL DEFAULT 0, \
                 issues_found BIGINT NOT NULL DEFAULT 0, \
                 last_error TEXT)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS performance_scan_state")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_error_groups_performance")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE error_groups DROP CONSTRAINT IF EXISTS error_groups_issue_type",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE error_groups DROP COLUMN IF EXISTS issue_type")
            .await?;
        Ok(())
    }
}
