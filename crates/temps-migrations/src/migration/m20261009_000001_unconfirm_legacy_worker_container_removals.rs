// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stop trusting `removed` on worker containers recorded before removal was
//! confirmed.
//!
//! Until this release, draining a node and failing one over recorded the
//! node's containers as `removed` after only stopping them, or without
//! reaching the node at all. From now on `removed` means Temps confirmed the
//! container is gone, and `retired` means it is out of routing but may still
//! exist. The two older meanings cannot be told apart in the data, so every
//! worker container recorded as `removed` becomes `retired`: the next node
//! removal or project cleanup checks it against the node, which confirms the
//! ones that really are gone. Local containers (no node) were always removed
//! before being recorded, and are left alone.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE deployment_containers SET status = 'retired' \
                 WHERE status = 'removed' AND node_id IS NOT NULL;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Irreversible by design: which rows were converted is not recorded,
        // and `retired` rows only cost a check against their node.
        Ok(())
    }
}
