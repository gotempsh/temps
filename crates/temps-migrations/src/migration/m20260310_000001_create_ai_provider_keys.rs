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
                    .table(AiProviderKeys::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiProviderKeys::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiProviderKeys::Provider)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiProviderKeys::DisplayName)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiProviderKeys::ApiKeyEncrypted)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiProviderKeys::BaseUrl).text())
                    .col(
                        ColumnDef::new(AiProviderKeys::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(AiProviderKeys::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiProviderKeys::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_ai_provider_keys_provider")
                    .table(AiProviderKeys::Table)
                    .col(AiProviderKeys::Provider)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_ai_provider_keys_is_active")
                    .table(AiProviderKeys::Table)
                    .col(AiProviderKeys::IsActive)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_ai_provider_keys_is_active")
                    .table(AiProviderKeys::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_ai_provider_keys_provider")
                    .table(AiProviderKeys::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(AiProviderKeys::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiProviderKeys {
    Table,
    Id,
    Provider,
    DisplayName,
    ApiKeyEncrypted,
    BaseUrl,
    IsActive,
    CreatedAt,
    UpdatedAt,
}
