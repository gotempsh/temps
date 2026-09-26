// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add a monotonic fencing token for control-plane overlay setup attempts.
//!
//! Setup performs privileged host and Docker mutations after reserving an
//! allocation. The generation prevents an older concurrent attempt from
//! publishing or withdrawing the readiness of a newer attempt.

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
                        ColumnDef::new(NetworkConfig::ControlPlaneSetupGeneration)
                            .big_integer()
                            .not_null()
                            .default(0),
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
                    .drop_column(NetworkConfig::ControlPlaneSetupGeneration)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum NetworkConfig {
    Table,
    ControlPlaneSetupGeneration,
}
