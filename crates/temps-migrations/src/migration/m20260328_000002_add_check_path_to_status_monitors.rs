// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add check_path column to status_monitors for custom health check paths
        // from .temps.yaml or user configuration
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("status_monitors"))
                    .add_column(
                        ColumnDef::new(Alias::new("check_path"))
                            .string()
                            .null()
                            .default("/"),
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
                    .table(Alias::new("status_monitors"))
                    .drop_column(Alias::new("check_path"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
