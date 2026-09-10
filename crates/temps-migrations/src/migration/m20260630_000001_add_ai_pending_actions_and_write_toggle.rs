// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration for the AI propose-then-confirm write-action feature.
//!
//! Two changes:
//!
//! 1. `projects.ai_write_actions_enabled` — per-project boolean flag (NOT NULL,
//!    default false) that gates whether the AI SRE may propose write actions.
//!    Existing rows default to false (opt-in; no surprise mutations).
//!
//! 2. `ai_pending_actions` — one row per proposed mutation, persisted until it
//!    is executed, rejected, or expires. The `params` / `result` columns use
//!    JSONB (json_binary in sea-orm) to store arbitrary structured data without
//!    a rigid schema, consistent with `ai_conversations.metadata`.
//!
//! Indexes created via raw DDL (SchemaManager's Index builder cannot express
//! DESC ordering or composite indexes with a DESC key):
//!   - unique on `public_id`  (already via UNIQUE KEY on the column)
//!   - btree on `conversation_id`
//!   - btree on `(project_id, status)` for the per-project status filter

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Per-project opt-in toggle for AI write actions.
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::AiWriteActionsEnabled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // 2. Pending action table.
        manager
            .create_table(
                Table::create()
                    .table(AiPendingActions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiPendingActions::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiPendingActions::PublicId)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(AiPendingActions::ConversationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiPendingActions::MessageId).big_integer())
                    .col(
                        ColumnDef::new(AiPendingActions::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiPendingActions::OperationId)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiPendingActions::Method).text().not_null())
                    .col(ColumnDef::new(AiPendingActions::Summary).text().not_null())
                    .col(
                        ColumnDef::new(AiPendingActions::Params)
                            .json_binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiPendingActions::RequiredPermission).text())
                    .col(
                        ColumnDef::new(AiPendingActions::Status)
                            .text()
                            .not_null()
                            .default("proposed"),
                    )
                    .col(ColumnDef::new(AiPendingActions::Result).json_binary())
                    .col(ColumnDef::new(AiPendingActions::Error).text())
                    .col(ColumnDef::new(AiPendingActions::CreatedBy).integer())
                    .col(ColumnDef::new(AiPendingActions::ConfirmedBy).integer())
                    .col(
                        ColumnDef::new(AiPendingActions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(AiPendingActions::ConfirmedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(AiPendingActions::ExecutedAt).timestamp_with_time_zone())
                    .to_owned(),
            )
            .await?;

        // Lookup indexes. `public_id` UNIQUE is already on the column above;
        // add the two btree indexes needed for common query patterns.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_ai_pending_actions_conversation \
                 ON ai_pending_actions (conversation_id); \
                 CREATE INDEX IF NOT EXISTS idx_ai_pending_actions_project_status \
                 ON ai_pending_actions (project_id, status);",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop indexes first (dropped implicitly with the table, but explicit is
        // cleaner and required if the table drop is done conditionally).
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS idx_ai_pending_actions_project_status; \
                 DROP INDEX IF EXISTS idx_ai_pending_actions_conversation;",
            )
            .await?;

        manager
            .drop_table(
                Table::drop()
                    .table(AiPendingActions::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::AiWriteActionsEnabled)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiPendingActions {
    Table,
    Id,
    PublicId,
    ConversationId,
    MessageId,
    ProjectId,
    OperationId,
    Method,
    Summary,
    Params,
    RequiredPermission,
    Status,
    Result,
    Error,
    CreatedBy,
    ConfirmedBy,
    CreatedAt,
    ConfirmedAt,
    ExecutedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    AiWriteActionsEnabled,
}
