//! Migration to add `default_model` to the `ai_provider_keys` table.
//!
//! Lets an operator pin which model each connected AI provider serves (e.g. a
//! local Ollama tag, or `gpt-4o-mini`), surfaced and editable in the AI
//! Providers settings UI. NULLABLE with no default — existing rows get NULL,
//! which means "fall back to the per-provider default" (`resolve_model`), so the
//! change is additive and backward-compatible.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiProviderKeys::Table)
                    .add_column(ColumnDef::new(AiProviderKeys::DefaultModel).text().null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiProviderKeys::Table)
                    .drop_column(AiProviderKeys::DefaultModel)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiProviderKeys {
    Table,
    DefaultModel,
}
