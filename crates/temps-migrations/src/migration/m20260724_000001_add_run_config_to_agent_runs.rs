//! Per-run AI configuration for autofixer runs.
//!
//! Adds `agent_runs.run_config` (nullable JSONB) holding the user-chosen
//! options a run was started with: provider, model, max_turns, and branch.
//! Stored on the run (not just resolved at execution time) so the run detail
//! UI can display exactly what a run executed with and prefill the
//! retry/start-over dialog. Null for historical rows and for generic agent
//! runs, whose config lives in `project_agents` / `ephemeral_yaml`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRuns::Table)
                    .add_column(ColumnDef::new(AgentRuns::RunConfig).json_binary().null())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRuns::Table)
                    .drop_column(AgentRuns::RunConfig)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AgentRuns {
    Table,
    RunConfig,
}
