// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .add_column(
                        ColumnDef::new(AiUsageLogs::IsByok)
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
                    .table(AiUsageLogs::Table)
                    .drop_column(AiUsageLogs::IsByok)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AiUsageLogs {
    Table,
    IsByok,
}
