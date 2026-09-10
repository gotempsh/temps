// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AiGatewayConfig::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiGatewayConfig::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiGatewayConfig::Scope)
                            .string_len(255)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(AiGatewayConfig::AllowedModels).json_binary())
                    .col(ColumnDef::new(AiGatewayConfig::MaxRequestsPerMinute).big_integer())
                    .col(ColumnDef::new(AiGatewayConfig::MaxCostPerMonthMicrocents).big_integer())
                    .col(
                        ColumnDef::new(AiGatewayConfig::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiGatewayConfig::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiGatewayConfig::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiGatewayConfig {
    Table,
    Id,
    Scope,
    AllowedModels,
    MaxRequestsPerMinute,
    MaxCostPerMonthMicrocents,
    CreatedAt,
    UpdatedAt,
}
