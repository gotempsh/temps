//! Adds `provisioning_step` and `provisioning_error` columns to
//! `service_members` so the UI can render a live timeline while a new
//! cluster member is being created in the background.
//!
//! The HTTP request that triggers `add_cluster_member` returns a 202 as
//! soon as the row is inserted; the actual container provisioning + DNS
//! registration runs in a tokio task that updates `provisioning_step` as
//! it walks each phase. The frontend polls the member row every second
//! and renders the steps as a checklist.
//!
//! ## Why a column, not an audit table
//!
//! The information is per-member, transient (we only care about the most
//! recent attempt), and bounded (≤ a dozen short strings). A separate
//! table would force a join on every member read and a row-per-step
//! retention policy we don't need. A pair of nullable columns is enough.
//!
//! ## Why nullable
//!
//! Existing rows from before this migration won't have a value, and
//! members created via the original synchronous code path (the
//! `initialize_cluster` flow used during cluster creation) never go
//! through this state machine. NULL means "no provisioning attempt
//! tracked here" — the UI falls back to the existing `status` column.

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
                    .add_column(
                        ColumnDef::new(ServiceMembers::ProvisioningStep)
                            .text()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(ServiceMembers::ProvisioningError)
                            .text()
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
                    .table(ServiceMembers::Table)
                    .drop_column(ServiceMembers::ProvisioningStep)
                    .drop_column(ServiceMembers::ProvisioningError)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ServiceMembers {
    Table,
    ProvisioningStep,
    ProvisioningError,
}
