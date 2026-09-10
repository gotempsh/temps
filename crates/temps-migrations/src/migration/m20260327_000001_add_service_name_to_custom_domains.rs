// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add service_name column to project_custom_domains for docker-compose service targeting
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("project_custom_domains"))
                    .add_column(ColumnDef::new(Alias::new("service_name")).string().null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("project_custom_domains"))
                    .drop_column(Alias::new("service_name"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
