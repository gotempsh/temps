// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE cron_monitors (
 id SERIAL PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 environment_id INTEGER REFERENCES environments(id) ON DELETE CASCADE,
 slug VARCHAR(128) NOT NULL, config JSONB NOT NULL, status TEXT NOT NULL DEFAULT 'unknown',
 consecutive_failures INTEGER NOT NULL DEFAULT 0, consecutive_successes INTEGER NOT NULL DEFAULT 0,
 alarm_id INTEGER REFERENCES alarms(id) ON DELETE SET NULL, alarm_pending BOOLEAN NOT NULL DEFAULT FALSE,
 next_expected_at TIMESTAMPTZ NOT NULL, next_check_at TIMESTAMPTZ NOT NULL, last_check_in_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX cron_monitors_scope ON cron_monitors(project_id, COALESCE(environment_id,0),slug);
CREATE INDEX cron_monitors_alarm_pending ON cron_monitors(id) WHERE alarm_pending;
CREATE INDEX cron_monitors_due ON cron_monitors(next_check_at);
CREATE TABLE cron_monitor_runs (
 id BIGSERIAL PRIMARY KEY, monitor_id INTEGER NOT NULL REFERENCES cron_monitors(id) ON DELETE CASCADE,
 check_in_id VARCHAR(64) NOT NULL, status TEXT NOT NULL CHECK(status IN ('in_progress','ok','error','missed','timeout')),
 started_at TIMESTAMPTZ NOT NULL, finished_at TIMESTAMPTZ, deadline_at TIMESTAMPTZ,
 duration DOUBLE PRECISION, release VARCHAR(256), environment VARCHAR(256), deployment_id INTEGER REFERENCES deployments(id) ON DELETE SET NULL,
 trace_id VARCHAR(32), span_id VARCHAR(16), UNIQUE(monitor_id,check_in_id)
);
CREATE INDEX cron_monitor_runs_history ON cron_monitor_runs(monitor_id,id DESC);
CREATE INDEX cron_monitor_runs_deadlines ON cron_monitor_runs(monitor_id,deadline_at) WHERE status='in_progress';
CREATE INDEX cron_monitor_runs_retention ON cron_monitor_runs(finished_at) WHERE finished_at IS NOT NULL;
"#).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE cron_monitor_runs; DROP TABLE cron_monitors")
            .await?;
        Ok(())
    }
}
