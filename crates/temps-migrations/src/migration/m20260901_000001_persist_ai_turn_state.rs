// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Makes an AI turn a server-owned resource rather than browser-owned state.
//!
//! A conversation can have at most one active turn. The opaque turn id makes
//! message submission idempotent and the persisted status lets a refreshed UI
//! reattach without pretending the harness stopped when only the SSE viewer
//! disconnected.

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
                    .add_column(
                        ColumnDef::new(AiConversations::TurnStatus)
                            .text()
                            .not_null()
                            .default("idle"),
                    )
                    .add_column(ColumnDef::new(AiConversations::ActiveTurnId).text())
                    .add_column(ColumnDef::new(AiConversations::LastTurnId).text())
                    .add_column(
                        ColumnDef::new(AiConversations::TurnStartedAt).timestamp_with_time_zone(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiConversations::Table)
                    .drop_column(AiConversations::TurnStartedAt)
                    .drop_column(AiConversations::ActiveTurnId)
                    .drop_column(AiConversations::LastTurnId)
                    .drop_column(AiConversations::TurnStatus)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AiConversations {
    Table,
    TurnStatus,
    ActiveTurnId,
    LastTurnId,
    TurnStartedAt,
}
