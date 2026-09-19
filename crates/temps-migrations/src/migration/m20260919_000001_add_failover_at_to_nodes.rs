// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record when an offline node's workloads were failed over.
//!
//! A node is marked offline after 90s without a heartbeat, and failover used to
//! run in that same tick: every single-replica environment on the node was
//! redeployed at once. A brief network partition between a worker and the
//! control plane was therefore enough to rebuild a whole node's worth of apps
//! that had never stopped serving traffic.
//!
//! Failover now waits for a grace period (`settings.multi_node
//! .node_failover_after_secs`) measured from the last heartbeat, which means it
//! no longer happens on the active->offline transition and needs its own
//! "already done" marker. Without one, every 60s health tick during a long
//! outage would re-trigger the redeploys. An in-process set would be lost on a
//! control-plane restart — exactly when a cluster is least stable — so the
//! marker lives on the row.
//!
//! NULL means "workloads have not been failed over for the current outage".
//! The health loop stamps it when failover runs and the heartbeat handler
//! clears it on recovery, so the column is self-correcting and needs no
//! backfill.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE nodes ADD COLUMN IF NOT EXISTS failover_at TIMESTAMPTZ NULL",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE nodes DROP COLUMN IF EXISTS failover_at")
            .await?;
        Ok(())
    }
}
