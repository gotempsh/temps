// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055: deployment replicas on workers through the workload plane.
//!
//! - `workload_instances.published_ports` (JSONB array of
//!   `{container_port, protocol, host_ip, host_port}`, default `[]`): the
//!   host ports a worker's node runtime published for the instance, as its
//!   heartbeat reports them. The control-plane proxy dials
//!   `host_ip:host_port`; before this, only the control plane's own driver
//!   could answer which port a replica got.
//!
//! Every existing row gets `[]`, which is what it had: no instance on a
//! worker published a port before deployment replicas could run there.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Also run by tests that build the plane's tables in an isolated schema
/// (after [`crate::WORKLOAD_TABLES_SQL`]).
pub const UP_SQL: &str = r#"
ALTER TABLE workload_instances
    ADD COLUMN IF NOT EXISTS published_ports JSONB NOT NULL DEFAULT '[]'::jsonb;
"#;

pub const DOWN_SQL: &str = r#"
ALTER TABLE workload_instances DROP COLUMN IF EXISTS published_ports;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}
