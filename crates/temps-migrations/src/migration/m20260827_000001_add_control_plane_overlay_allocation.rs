// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist the control plane's overlay allocation in the cluster network
//! singleton. The control plane intentionally is not a row in `nodes` because
//! node_id = NULL is the long-standing marker for locally managed workloads.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(NetworkConfig::Table)
                    .add_column(
                        ColumnDef::new(NetworkConfig::ControlPlaneComputeCidr)
                            .text()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(NetworkConfig::ControlPlaneUnderlayAddress)
                            .text()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(NetworkConfig::ControlPlaneOverlayReady)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(NetworkConfig::Table)
                    .drop_column(NetworkConfig::ControlPlaneOverlayReady)
                    .drop_column(NetworkConfig::ControlPlaneUnderlayAddress)
                    .drop_column(NetworkConfig::ControlPlaneComputeCidr)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum NetworkConfig {
    Table,
    ControlPlaneComputeCidr,
    ControlPlaneUnderlayAddress,
    ControlPlaneOverlayReady,
}
