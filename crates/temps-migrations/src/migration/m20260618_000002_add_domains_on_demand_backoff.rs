//! Migration to add `on_demand_backoff_until` to the `domains` table (ADR-018).
//!
//! Backs the per-host negative cache (ADR §4 Layer 2): when an on-demand TLS
//! issuance fails for a hostname, this column is set to `now + exponential_delay`
//! and the proxy's `certificate_callback` refuses to re-enqueue a job for that
//! hostname until the timestamp elapses. This stops a single misconfigured or
//! flapping domain from burning Let's Encrypt failed-authorization attempts.
//!
//! The column is NULLABLE with no default — existing rows get NULL on upgrade
//! (no active backoff), preserving today's behaviour exactly. Additive-only and
//! backward-compatible with the N-1 proxy binary (ADR §9). The on-demand
//! `status` values (`on_demand_pending` / `on_demand_issuing` / `on_demand_failed`)
//! reuse the existing string `status` column and need no schema change.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Domains::Table)
                    .add_column(
                        ColumnDef::new(Domains::OnDemandBackoffUntil)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Domains::Table)
                    .drop_column(Domains::OnDemandBackoffUntil)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Domains {
    Table,
    OnDemandBackoffUntil,
}
