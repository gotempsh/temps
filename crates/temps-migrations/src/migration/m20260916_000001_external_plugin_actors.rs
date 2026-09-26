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
                    .table(ExternalPluginActors::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ExternalPluginActors::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginActors::PluginName)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginActors::BinarySha256)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginActors::SourceIdentity)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginActors::Active)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginActors::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginActors::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(ExternalPluginGrants::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ExternalPluginGrants::ActorId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginGrants::Permissions)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginGrants::AiDailyCallLimit)
                            .integer()
                            .not_null()
                            .default(100),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginGrants::AiMaxOutputTokens)
                            .integer()
                            .not_null()
                            .default(1024),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginGrants::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ExternalPluginGrants::Table, ExternalPluginGrants::ActorId)
                            .to(ExternalPluginActors::Table, ExternalPluginActors::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(ExternalPluginAiUsage::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ExternalPluginAiUsage::ActorId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginAiUsage::UsageDate)
                            .date()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalPluginAiUsage::Calls)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .primary_key(
                        Index::create()
                            .col(ExternalPluginAiUsage::ActorId)
                            .col(ExternalPluginAiUsage::UsageDate),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ExternalPluginAiUsage::Table, ExternalPluginAiUsage::ActorId)
                            .to(ExternalPluginActors::Table, ExternalPluginActors::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ExternalPluginAiUsage::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ExternalPluginGrants::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ExternalPluginActors::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ExternalPluginActors {
    Table,
    Id,
    PluginName,
    BinarySha256,
    SourceIdentity,
    Active,
    CreatedAt,
    UpdatedAt,
}
#[derive(DeriveIden)]
enum ExternalPluginGrants {
    Table,
    ActorId,
    Permissions,
    AiDailyCallLimit,
    AiMaxOutputTokens,
    UpdatedAt,
}
#[derive(DeriveIden)]
enum ExternalPluginAiUsage {
    Table,
    ActorId,
    UsageDate,
    Calls,
}
