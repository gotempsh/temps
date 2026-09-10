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
                    .table(OauthStates::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OauthStates::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OauthStates::State)
                            .string_len(128)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(OauthStates::UserId).integer().not_null())
                    .col(ColumnDef::new(OauthStates::ProviderId).integer().not_null())
                    .col(
                        ColumnDef::new(OauthStates::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OauthStates::CreatedAt)
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
                    .if_not_exists()
                    .name("idx_oauth_states_expires_at")
                    .table(OauthStates::Table)
                    .col(OauthStates::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OauthStates::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum OauthStates {
    Table,
    Id,
    State,
    UserId,
    ProviderId,
    ExpiresAt,
    CreatedAt,
}
