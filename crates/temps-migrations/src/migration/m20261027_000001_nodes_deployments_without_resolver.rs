// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record, per node, which deployments run without the cluster resolver.
//!
//! A container's nameservers are fixed when it is created, so containers
//! started before cluster DNS was turned on cannot resolve `*.temps.local`
//! until they are redeployed. Each heartbeat reports the deployment ids
//! whose containers lack the node's resolver; the console lists them with a
//! redeploy action.
//!
//! A JSON array of deployment ids, `NULL` until the node first reports (an
//! agent binary older than this column, or a node that has not beaten yet).
//! A current snapshot rather than history, like the other resolver health
//! columns added by `m20260825_000001_add_dns_resolver_health_to_nodes`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE nodes ADD COLUMN IF NOT EXISTS deployments_without_resolver JSONB;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE nodes DROP COLUMN IF EXISTS deployments_without_resolver;",
            )
            .await?;
        Ok(())
    }
}
