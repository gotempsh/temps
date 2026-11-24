// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Files attached to Temps assistant chats.
//!
//! Application chats keep attachments in a persistent sandbox workspace;
//! assistant chats have none, so their (small, bounded) files are stored here
//! and cascade away with the conversation. The index serves the two lookups:
//! a conversation's files, and the sweep of uploads that were never sent.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AiChatAttachments::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiChatAttachments::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiChatAttachments::PublicId)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(AiChatAttachments::ConversationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiChatAttachments::Name)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiChatAttachments::MimeType)
                            .string_len(128)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiChatAttachments::SizeBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiChatAttachments::Content)
                            .binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiChatAttachments::SentAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(AiChatAttachments::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_chat_attachments_conversation")
                            .from(AiChatAttachments::Table, AiChatAttachments::ConversationId)
                            .to(AiConversations::Table, AiConversations::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_chat_attachments_conversation")
                    .table(AiChatAttachments::Table)
                    .col(AiChatAttachments::ConversationId)
                    .col(AiChatAttachments::SentAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiChatAttachments::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AiChatAttachments {
    Table,
    Id,
    PublicId,
    ConversationId,
    Name,
    MimeType,
    SizeBytes,
    Content,
    SentAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum AiConversations {
    Table,
    Id,
}
