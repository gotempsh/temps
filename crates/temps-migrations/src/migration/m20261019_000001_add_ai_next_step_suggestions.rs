// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether the assistant offers next-step suggestions after an answer.
//!
//! Each user has a default (`ai_user_preferences.next_step_suggestions`), and a
//! chat may override it (`ai_conversations.next_step_suggestions`, `NULL` to
//! follow the default). The value is `off`, `ask` or `always`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiConversations::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(AiConversations::NextStepSuggestions).string_len(16),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(AiUserPreferences::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiUserPreferences::UserId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiUserPreferences::NextStepSuggestions)
                            .string_len(16)
                            .not_null()
                            .default("ask"),
                    )
                    .col(
                        ColumnDef::new(AiUserPreferences::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiUserPreferences::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_user_preferences_user")
                            .from(AiUserPreferences::Table, AiUserPreferences::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiUserPreferences::Table).to_owned())
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(AiConversations::Table)
                    .drop_column(AiConversations::NextStepSuggestions)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AiConversations {
    Table,
    NextStepSuggestions,
}

#[derive(DeriveIden)]
enum AiUserPreferences {
    Table,
    UserId,
    NextStepSuggestions,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
