// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record every published TCP port of a deployment container.
//!
//! `container_port`/`host_port` hold a single mapping, which limited a Docker
//! Compose service to one public URL: when Temps runs on the host, the proxy
//! can only reach a service port through its live host mapping, and only one
//! was persisted. `port_bindings` stores all `{container_port, host_port}`
//! pairs Docker reported so each configured public port routes to its own
//! mapping. NULL on rows written before this column existed; readers fall back
//! to the legacy single mapping.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE deployment_containers \
             ADD COLUMN IF NOT EXISTS port_bindings JSONB",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE deployment_containers \
             DROP COLUMN IF EXISTS port_bindings",
        )
        .await?;
        Ok(())
    }
}
