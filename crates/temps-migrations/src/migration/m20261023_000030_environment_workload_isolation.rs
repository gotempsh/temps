// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 10: Firecracker for deployments.
//!
//! `environments.workload_isolation` (`process` | `micro_vm`, default
//! `process`): the isolation an environment's plane-run deployments request.
//! `micro_vm` places them only on a node whose runtime driver provides
//! microVM isolation (Firecracker), and refuses the deployment with the
//! scheduler's reason when none does. Every existing environment keeps
//! `process`, which is the container path it already runs on.

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
ALTER TABLE environments
    ADD COLUMN IF NOT EXISTS workload_isolation TEXT NOT NULL DEFAULT 'process';
ALTER TABLE environments
    DROP CONSTRAINT IF EXISTS environments_workload_isolation_valid;
ALTER TABLE environments
    ADD CONSTRAINT environments_workload_isolation_valid
        CHECK (workload_isolation IN ('process', 'micro_vm'));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_workload_isolation_valid;
ALTER TABLE environments DROP COLUMN IF EXISTS workload_isolation;
"#,
            )
            .await?;
        Ok(())
    }
}
