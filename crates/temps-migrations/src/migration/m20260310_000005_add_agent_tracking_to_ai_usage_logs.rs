// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add conversation_id column
        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .add_column(ColumnDef::new(AiUsageLogs::ConversationId).text())
                    .to_owned(),
            )
            .await?;

        // Add request_id column
        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .add_column(ColumnDef::new(AiUsageLogs::RequestId).text())
                    .to_owned(),
            )
            .await?;

        // Add trace_id column
        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .add_column(ColumnDef::new(AiUsageLogs::TraceId).text())
                    .to_owned(),
            )
            .await?;

        // Add tags column as TEXT[] with default '{}' via raw SQL
        // (SeaORM schema builder doesn't support Postgres array types)
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE ai_usage_logs ADD COLUMN tags TEXT[] NOT NULL DEFAULT '{}'",
        )
        .await?;

        // Partial index on conversation_id (only non-null rows)
        db.execute_unprepared(
            "CREATE INDEX idx_ai_usage_logs_conversation_id ON ai_usage_logs (conversation_id) WHERE conversation_id IS NOT NULL",
        )
        .await?;

        // GIN index on tags for array containment queries
        db.execute_unprepared(
            "CREATE INDEX idx_ai_usage_logs_tags ON ai_usage_logs USING GIN (tags)",
        )
        .await?;

        // Partial index on trace_id (only non-null rows)
        db.execute_unprepared(
            "CREATE INDEX idx_ai_usage_logs_trace_id ON ai_usage_logs (trace_id) WHERE trace_id IS NOT NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Drop indexes first
        db.execute_unprepared("DROP INDEX IF EXISTS idx_ai_usage_logs_trace_id")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_ai_usage_logs_tags")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_ai_usage_logs_conversation_id")
            .await?;

        // Drop columns
        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .drop_column(AiUsageLogs::TraceId)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .drop_column(AiUsageLogs::RequestId)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .drop_column(AiUsageLogs::Tags)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .drop_column(AiUsageLogs::ConversationId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiUsageLogs {
    Table,
    ConversationId,
    Tags,
    RequestId,
    TraceId,
}
