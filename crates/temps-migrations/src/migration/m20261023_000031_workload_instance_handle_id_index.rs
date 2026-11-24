// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 10: find a workload instance by its runtime id.
//!
//! A deployment replica in a microVM is recorded in `deployment_containers`
//! by its runtime id, like a container, but the host's Docker cannot act on
//! it. The deploy path looks the instance up by that id
//! (`workload_instances.handle->>'id'`) to route the operation through the
//! workload plane. This index keeps that lookup O(log n) instead of a scan
//! of every instance the plane ever ran.

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
CREATE INDEX IF NOT EXISTS workload_instances_handle_id
    ON workload_instances ((handle->>'id'));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS workload_instances_handle_id;")
            .await?;
        Ok(())
    }
}
