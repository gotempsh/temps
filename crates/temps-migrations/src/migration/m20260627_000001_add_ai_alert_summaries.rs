//! Migration to add `ai_alert_summaries_enabled` to the `projects` table
//! (ADR-021 Tier 2).
//!
//! Per-project opt-in for AI summarization of metric alert notifications. The
//! column is NULLABLE with no default — existing rows get NULL on upgrade, which
//! means "off": the deterministic Tier-1 humanized text is used. Set to `true`
//! to enrich this project's alerts with the configured AI provider (best-effort;
//! falls back to the deterministic text when no provider is configured).
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
                        ColumnDef::new(Projects::AiAlertSummariesEnabled)
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
                    .drop_column(Projects::AiAlertSummariesEnabled)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    AiAlertSummariesEnabled,
}
