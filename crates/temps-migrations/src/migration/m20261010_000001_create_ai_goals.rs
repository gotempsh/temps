// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Goals the Temps assistant tracks on a user's behalf, and heals when they
//! fail.
//!
//! A goal is one observable condition ("this deployment is healthy") expressed
//! as a read-only API operation plus the values that mean success and failure.
//! A background watcher evaluates it; a failed check starts a bounded fix turn
//! in the owning conversation. The index serves the watcher's only query:
//! "which open goals are due".

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AiGoals::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiGoals::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiGoals::PublicId)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(AiGoals::ConversationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiGoals::OwnerUserId).integer().not_null())
                    .col(ColumnDef::new(AiGoals::Title).string_len(200).not_null())
                    .col(
                        ColumnDef::new(AiGoals::CheckOperation)
                            .string_len(200)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiGoals::CheckParams)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiGoals::SuccessPath)
                            .string_len(200)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiGoals::SuccessValues)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiGoals::FailureValues)
                            .json_binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiGoals::Status).string_len(32).not_null())
                    .col(
                        ColumnDef::new(AiGoals::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AiGoals::MaxAttempts)
                            .integer()
                            .not_null()
                            .default(3),
                    )
                    .col(ColumnDef::new(AiGoals::LastObservation).text())
                    .col(ColumnDef::new(AiGoals::LastCheckedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(AiGoals::NextCheckAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiGoals::DeadlineAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiGoals::AuthSessionId).integer())
                    .col(
                        ColumnDef::new(AiGoals::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiGoals::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_goals_conversation")
                            .from(AiGoals::Table, AiGoals::ConversationId)
                            .to(AiConversations::Table, AiConversations::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_goals_due")
                    .table(AiGoals::Table)
                    .col(AiGoals::Status)
                    .col(AiGoals::NextCheckAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_goals_conversation")
                    .table(AiGoals::Table)
                    .col(AiGoals::ConversationId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiGoals::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AiGoals {
    Table,
    Id,
    PublicId,
    ConversationId,
    OwnerUserId,
    Title,
    CheckOperation,
    CheckParams,
    SuccessPath,
    SuccessValues,
    FailureValues,
    Status,
    Attempts,
    MaxAttempts,
    LastObservation,
    LastCheckedAt,
    NextCheckAt,
    DeadlineAt,
    AuthSessionId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum AiConversations {
    Table,
    Id,
}
