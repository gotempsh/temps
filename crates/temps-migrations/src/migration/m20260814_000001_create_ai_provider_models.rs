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
                    .table(AiProviderModels::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiProviderModels::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiProviderModels::ProviderKeyId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiProviderModels::ModelId)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiProviderModels::DisplayName)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiProviderModels::Source)
                            .string_len(20)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiProviderModels::IsAvailable)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(AiProviderModels::IsEnabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(ColumnDef::new(AiProviderModels::OwnedBy).string_len(255))
                    .col(ColumnDef::new(AiProviderModels::Metadata).json_binary())
                    .col(ColumnDef::new(AiProviderModels::LastSeenAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(AiProviderModels::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiProviderModels::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_provider_models_key")
                            .from(AiProviderModels::Table, AiProviderModels::ProviderKeyId)
                            .to(AiProviderKeys::Table, AiProviderKeys::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("uidx_ai_provider_models_key_model")
                    .table(AiProviderModels::Table)
                    .col(AiProviderModels::ProviderKeyId)
                    .col(AiProviderModels::ModelId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_provider_models_key_enabled")
                    .table(AiProviderModels::Table)
                    .col(AiProviderModels::ProviderKeyId)
                    .col(AiProviderModels::IsEnabled)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiProviderModels::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AiProviderModels {
    Table,
    Id,
    ProviderKeyId,
    ModelId,
    DisplayName,
    Source,
    IsAvailable,
    IsEnabled,
    OwnedBy,
    Metadata,
    LastSeenAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum AiProviderKeys {
    Table,
    Id,
}
