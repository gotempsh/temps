// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credentials the user hands the assistant without the model seeing them.
//!
//! When an assistant action needs a secret (an access key, a token) the model
//! asks for a *slot*; the user types the value into a form the console renders,
//! and the value lands here, encrypted. The model only ever holds the slot's
//! public id. A slot is bound to one user, one conversation, one operation and
//! one request field, and it expires: nothing here is a general-purpose vault.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AiSecretSlots::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiSecretSlots::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiSecretSlots::PublicId)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(AiSecretSlots::ConversationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiSecretSlots::UserId).integer().not_null())
                    .col(
                        ColumnDef::new(AiSecretSlots::OperationId)
                            .string_len(160)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiSecretSlots::Field)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiSecretSlots::Label)
                            .string_len(200)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiSecretSlots::Status)
                            .string_len(16)
                            .not_null()
                            .default("awaiting"),
                    )
                    .col(ColumnDef::new(AiSecretSlots::Ciphertext).text())
                    .col(
                        ColumnDef::new(AiSecretSlots::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiSecretSlots::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(AiSecretSlots::FilledAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_secret_slots_conversation")
                            .from(AiSecretSlots::Table, AiSecretSlots::ConversationId)
                            .to(AiConversations::Table, AiConversations::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_secret_slots_conversation")
                    .table(AiSecretSlots::Table)
                    .col(AiSecretSlots::ConversationId)
                    .col(AiSecretSlots::Status)
                    .to_owned(),
            )
            .await?;
        // The sweep that erases expired values reads by expiry.
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_secret_slots_expires_at")
                    .table(AiSecretSlots::Table)
                    .col(AiSecretSlots::ExpiresAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiSecretSlots::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AiSecretSlots {
    Table,
    Id,
    PublicId,
    ConversationId,
    UserId,
    OperationId,
    Field,
    Label,
    Status,
    Ciphertext,
    ExpiresAt,
    CreatedAt,
    FilledAt,
}

#[derive(DeriveIden)]
enum AiConversations {
    Table,
    Id,
}
