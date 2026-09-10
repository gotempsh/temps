// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add sleeping column to environments table.
        // When true, the environment's containers have been stopped due to inactivity
        // and will be started on the next incoming request.
        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .add_column(
                        ColumnDef::new(Environments::Sleeping)
                            .boolean()
                            .not_null()
                            .default(false),
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
                    .table(Environments::Table)
                    .drop_column(Environments::Sleeping)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    Sleeping,
}
