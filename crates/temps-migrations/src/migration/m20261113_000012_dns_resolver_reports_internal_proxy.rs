// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How the control plane serves `*.temps.local` HTTP calls to its own
//! containers (ADR-104 D4), stored with node 0's DNS report.
//!
//! A worker reports its internal routing in the heartbeat's
//! `capacity.internal_proxy` (ADR-098 D5). The control plane has no `nodes`
//! row and no heartbeat, so its proxy process writes the same document here
//! next to the resolver report it already writes. `NULL`, the value of every
//! existing row, means "not reported": a worker's row never sets it, and a
//! control plane older than ADR-104 did not run the proxy. Nothing to backfill.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE dns_resolver_reports ADD COLUMN IF NOT EXISTS internal_proxy jsonb NULL",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE dns_resolver_reports DROP COLUMN IF EXISTS internal_proxy")
            .await?;
        Ok(())
    }
}
