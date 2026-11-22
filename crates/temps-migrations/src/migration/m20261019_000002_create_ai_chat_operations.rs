// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The long-running operations a chat started: deployments, backups and
//! restores. The chat's operations button lists them with their live state, so
//! the user can see what is still in flight without scrolling back to the
//! message that started it.
//!
//! A row is only a pointer (which deployment, backup or restore run); the state
//! is always read from that record, never copied here.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AiChatOperations::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiChatOperations::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiChatOperations::ConversationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiChatOperations::Kind)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiChatOperations::OperationId)
                            .string_len(160)
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiChatOperations::TargetId).integer())
                    .col(ColumnDef::new(AiChatOperations::ProjectId).integer())
                    .col(ColumnDef::new(AiChatOperations::EnvironmentId).integer())
                    .col(ColumnDef::new(AiChatOperations::ServiceId).integer())
                    .col(
                        ColumnDef::new(AiChatOperations::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_chat_operations_conversation")
                            .from(AiChatOperations::Table, AiChatOperations::ConversationId)
                            .to(AiConversations::Table, AiConversations::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // The chat's list reads its newest operations.
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_chat_operations_conversation_created")
                    .table(AiChatOperations::Table)
                    .col(AiChatOperations::ConversationId)
                    .col(AiChatOperations::CreatedAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiChatOperations::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AiChatOperations {
    Table,
    Id,
    ConversationId,
    Kind,
    OperationId,
    TargetId,
    ProjectId,
    EnvironmentId,
    ServiceId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum AiConversations {
    Table,
    Id,
}
