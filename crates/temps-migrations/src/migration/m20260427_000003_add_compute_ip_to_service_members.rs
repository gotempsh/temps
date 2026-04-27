//! Adds `service_members.compute_ip` — the per-container overlay IP that
//! the deployer/lifecycle hook writes after `docker create` returns, and
//! that the proxy + DNS registry both read.
//!
//! ## Why a column, not a join
//!
//! Container IPs come from `docker inspect NetworkSettings.Networks.temps-overlay.IPAddress`.
//! That's an out-of-band call — we don't want the proxy or the DNS layer
//! making it on every read. Storing the IP at registration time gives one
//! authoritative source the proxy and the DNS registry both read from
//! cache (existing route-table cache + sync long-poll).
//!
//! ## Why nullable
//!
//! 1. Single-host clusters don't bring up the overlay → no compute_ip.
//! 2. Pre-overlay deployments and members from before this migration
//!    have NULL until the next start cycle.
//! 3. The lifecycle hook is "best-effort" — if the inspect call fails
//!    we keep the member alive without DNS rather than refusing to start.
//!
//! Consumers (proxy, DNS reconciler) treat NULL as "fall back to legacy
//! single-host routing", matching today's behaviour.
//!
//! ## Why no FK / no index
//!
//! `compute_ip` is just an IP literal — there's nothing to FK to. We don't
//! need a secondary index because every read is `WHERE service_id = ?`
//! (already indexed via the existing service-member relationship), then
//! filters by `compute_ip IS NOT NULL` in memory.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ServiceMembers::Table)
                    .add_column(ColumnDef::new(ServiceMembers::ComputeIp).text().null())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ServiceMembers::Table)
                    .drop_column(ServiceMembers::ComputeIp)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ServiceMembers {
    Table,
    ComputeIp,
}
