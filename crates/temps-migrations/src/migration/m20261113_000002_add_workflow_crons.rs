// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cron schedules that start a durable workflow (ADR-106 §9, M2).
//!
//! A `.temps.yaml` cron with `workflow:` becomes a `crons` row in mode
//! `workflow`: `workflow_name` is the workflow it starts and
//! `workflow_input` the input each run gets, as written in the repository (so
//! not a secret, like a job cron's `command`). Each fire is a
//! `cron_executions` row whose `workflow_run_id` is the run it started, so
//! the history links to the run and the concurrency policy can tell whether
//! the previous run is still active. Every column is nullable and NULL for
//! existing rows: `http` and `job` crons are unchanged. `workflow_run_id`
//! carries no foreign key, as runs are deleted by retention while the
//! execution history stays. `crons_mode_valid` admits `workflow`; `down`
//! refuses while a live workflow cron exists rather than delete it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE crons ADD COLUMN IF NOT EXISTS workflow_name TEXT NULL
    CHECK (workflow_name IS NULL OR char_length(workflow_name) BETWEEN 1 AND 128);
ALTER TABLE crons ADD COLUMN IF NOT EXISTS workflow_input JSONB NULL;
ALTER TABLE cron_executions ADD COLUMN IF NOT EXISTS workflow_run_id UUID NULL UNIQUE;
ALTER TABLE crons DROP CONSTRAINT IF EXISTS crons_mode_valid;
ALTER TABLE crons ADD CONSTRAINT crons_mode_valid CHECK (mode IN ('http', 'job', 'workflow'));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let live = db
            .query_one(sea_orm::Statement::from_string(
                manager.get_database_backend(),
                "SELECT count(*)::BIGINT AS live FROM crons WHERE mode = 'workflow' AND deleted_at IS NULL",
            ))
            .await?
            .map(|row| row.try_get::<i64>("", "live"))
            .transpose()?
            .unwrap_or(0);
        if live > 0 {
            return Err(DbErr::Migration(format!(
                "cannot remove workflow crons: {live} live cron(s) in mode 'workflow' still exist; \
                 remove them from .temps.yaml and redeploy (or delete them) first"
            )));
        }
        db.execute_unprepared(
            r#"
UPDATE crons SET mode = 'http' WHERE mode = 'workflow';
ALTER TABLE crons DROP CONSTRAINT IF EXISTS crons_mode_valid;
ALTER TABLE crons ADD CONSTRAINT crons_mode_valid CHECK (mode IN ('http', 'job'));
ALTER TABLE cron_executions DROP COLUMN IF EXISTS workflow_run_id;
ALTER TABLE crons DROP COLUMN IF EXISTS workflow_input;
ALTER TABLE crons DROP COLUMN IF EXISTS workflow_name;
"#,
        )
        .await?;
        Ok(())
    }
}
