// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-operation hourly latency rollups (ADR 111, D2).
//!
//! `operation_rollups` holds one row per project, service, operation and
//! hour: call and error counts and the latency percentiles of that hour.
//! A background job writes it from the span-stats report, at most a fixed
//! number of operations per project and hour (span names are user
//! controlled); `operation_rollup_state` records where the job stopped and
//! how many operations of the last hour did not fit, so the console can say
//! so. Trends and regressions after a deployment read only these rows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS operation_rollups ( \
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                 service_name TEXT NOT NULL, \
                 span_name TEXT NOT NULL, \
                 bucket_start TIMESTAMPTZ NOT NULL, \
                 span_count BIGINT NOT NULL, \
                 error_count BIGINT NOT NULL, \
                 avg_ms DOUBLE PRECISION NOT NULL, \
                 p50_ms DOUBLE PRECISION NOT NULL, \
                 p95_ms DOUBLE PRECISION NOT NULL, \
                 p99_ms DOUBLE PRECISION NOT NULL, \
                 PRIMARY KEY (project_id, service_name, span_name, bucket_start))",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_operation_rollups_bucket \
             ON operation_rollups (project_id, bucket_start)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS operation_rollup_state ( \
                 project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE, \
                 rolled_until TIMESTAMPTZ NOT NULL, \
                 last_run_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 tracked_operations INTEGER NOT NULL DEFAULT 0, \
                 overflow_operations INTEGER NOT NULL DEFAULT 0, \
                 last_error TEXT)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS operation_rollup_state")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS operation_rollups")
            .await?;
        Ok(())
    }
}
