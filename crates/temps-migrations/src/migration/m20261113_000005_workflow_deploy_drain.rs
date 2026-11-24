// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Opt-in deployment draining pauses new invocations with an expiring lease.
//! Existing sleeping/waiting runs replay on the new deployment afterwards.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self,manager:&SchemaManager)->Result<(),DbErr> {
        manager.get_connection().execute_unprepared("ALTER TABLE workflow_environment_settings ADD COLUMN drain_timeout_secs INTEGER NOT NULL DEFAULT 0 CHECK(drain_timeout_secs BETWEEN 0 AND 3600);
            CREATE TABLE workflow_deployment_drains(environment_id INTEGER PRIMARY KEY REFERENCES environments(id) ON DELETE CASCADE, deployment_id INTEGER NOT NULL REFERENCES deployments(id) ON DELETE CASCADE, token UUID NOT NULL, expires_at TIMESTAMPTZ NOT NULL)").await?;Ok(())
    }
    async fn down(&self,manager:&SchemaManager)->Result<(),DbErr> {
        manager.get_connection().execute_unprepared("DROP TABLE workflow_deployment_drains; ALTER TABLE workflow_environment_settings DROP COLUMN drain_timeout_secs").await?;Ok(())
    }
}
