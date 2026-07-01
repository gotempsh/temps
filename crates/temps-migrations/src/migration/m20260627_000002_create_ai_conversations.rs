//! Migration for persistent AI debugging conversations (ADR-023).
//!
//! Two tables — a generic, entity-agnostic conversation store keyed by a
//! polymorphic `(context_type, context_id)` so any interaction (a deployment
//! failure first, alerts/error-groups later) gets one resumable chat.
//! `ai_conversations` is one row per chat (project + context); `ai_messages`
//! holds the turns (role/content), replayed as history each turn. Plus a
//! per-project opt-in toggle `projects.ai_debug_chat_enabled` (NULL/false = off).
//! Generalizes the dormant `workspace_messages` shape.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AiConversations::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiConversations::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiConversations::PublicId)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(AiConversations::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiConversations::ContextType)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiConversations::ContextId).text().not_null())
                    .col(ColumnDef::new(AiConversations::Title).text())
                    .col(
                        ColumnDef::new(AiConversations::Status)
                            .text()
                            .not_null()
                            .default("active"),
                    )
                    .col(ColumnDef::new(AiConversations::CreatedBy).integer())
                    .col(ColumnDef::new(AiConversations::Metadata).json_binary())
                    .col(
                        ColumnDef::new(AiConversations::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiConversations::LastActivityAt)
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
                    .table(AiMessages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiMessages::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiMessages::ConversationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiMessages::Role).text().not_null())
                    .col(ColumnDef::new(AiMessages::Content).text().not_null())
                    .col(ColumnDef::new(AiMessages::Metadata).json_binary())
                    .col(ColumnDef::new(AiMessages::TokensIn).integer())
                    .col(ColumnDef::new(AiMessages::TokensOut).integer())
                    .col(ColumnDef::new(AiMessages::CostMicrocents).big_integer())
                    .col(
                        ColumnDef::new(AiMessages::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Context lookup ("open the chat for this deployment"), message ordering,
        // and the FK (cascade delete a conversation's messages).
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_ai_conversations_context \
                 ON ai_conversations (project_id, context_type, context_id); \
                 CREATE INDEX IF NOT EXISTS idx_ai_messages_conversation \
                 ON ai_messages (conversation_id, created_at); \
                 ALTER TABLE ai_messages \
                 ADD CONSTRAINT fk_ai_messages_conversation \
                 FOREIGN KEY (conversation_id) REFERENCES ai_conversations (id) ON DELETE CASCADE;",
            )
            .await?;

        // Per-project opt-in for the debugging chat (NULL/false = off).
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::AiDebugChatEnabled)
                            .boolean()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::AiDebugChatEnabled)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(AiMessages::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AiConversations::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiConversations {
    Table,
    Id,
    PublicId,
    ProjectId,
    ContextType,
    ContextId,
    Title,
    Status,
    CreatedBy,
    Metadata,
    CreatedAt,
    LastActivityAt,
}

#[derive(DeriveIden)]
enum AiMessages {
    Table,
    Id,
    ConversationId,
    Role,
    Content,
    Metadata,
    TokensIn,
    TokensOut,
    CostMicrocents,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    AiDebugChatEnabled,
}
