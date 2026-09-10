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
                    .table(AiUsageLogs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiUsageLogs::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiUsageLogs::Timestamp)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(AiUsageLogs::UserId).integer())
                    .col(
                        ColumnDef::new(AiUsageLogs::Provider)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiUsageLogs::Model)
                            .string_len(100)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiUsageLogs::InputTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AiUsageLogs::OutputTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AiUsageLogs::LatencyMs)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AiUsageLogs::EstimatedCostMicrocents)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AiUsageLogs::Status)
                            .small_integer()
                            .not_null()
                            .default(200),
                    )
                    .col(
                        ColumnDef::new(AiUsageLogs::IsStreaming)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Index for querying by time range
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_usage_logs_timestamp")
                    .table(AiUsageLogs::Table)
                    .col(AiUsageLogs::Timestamp)
                    .to_owned(),
            )
            .await?;

        // Index for querying by provider
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_usage_logs_provider")
                    .table(AiUsageLogs::Table)
                    .col(AiUsageLogs::Provider)
                    .to_owned(),
            )
            .await?;

        // Index for querying by user
        manager
            .create_index(
                Index::create()
                    .name("idx_ai_usage_logs_user_id")
                    .table(AiUsageLogs::Table)
                    .col(AiUsageLogs::UserId)
                    .to_owned(),
            )
            .await?;

        // Try to convert to TimescaleDB hypertable (graceful fallback if TimescaleDB not available).
        // Wrapped in PL/pgSQL exception block so a failure doesn't abort the transaction.
        let db = manager.get_connection();
        let _ = db
            .execute_unprepared(
                "DO $$ BEGIN PERFORM create_hypertable('ai_usage_logs', 'timestamp', if_not_exists => TRUE, migrate_data => TRUE); EXCEPTION WHEN OTHERS THEN NULL; END $$",
            )
            .await;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_ai_usage_logs_user_id")
                    .table(AiUsageLogs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_ai_usage_logs_provider")
                    .table(AiUsageLogs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_ai_usage_logs_timestamp")
                    .table(AiUsageLogs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(AiUsageLogs::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiUsageLogs {
    Table,
    Id,
    Timestamp,
    UserId,
    Provider,
    Model,
    InputTokens,
    OutputTokens,
    LatencyMs,
    EstimatedCostMicrocents,
    Status,
    IsStreaming,
}
