//! Migration to add `ai_api_traffic_summary_enabled` to the `projects` table.
//!
//! Per-project opt-in for AI summarization of API traffic analytics. The column
//! is NULLABLE with no default — existing rows get NULL on upgrade, which means
//! "off". Set to `true` to generate an AI summary for the project's
//! `GET /projects/{id}/api-analytics/summary` endpoint. Falls back gracefully
//! when no AI provider is configured (returns `null` summary, never an error).
//! Additive-only and backward-compatible.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::AiApiTrafficSummaryEnabled)
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
                    .drop_column(Projects::AiApiTrafficSummaryEnabled)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    AiApiTrafficSummaryEnabled,
}
