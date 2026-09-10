// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add autofixer-specific columns to agent_runs
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRuns::Table)
                    .add_column(ColumnDef::new(AgentRuns::Phase).string_len(20).null())
                    .add_column(ColumnDef::new(AgentRuns::Analysis).text().null())
                    .add_column(ColumnDef::new(AgentRuns::UserContext).text().null())
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
                    .drop_column(AgentRuns::Phase)
                    .drop_column(AgentRuns::Analysis)
                    .drop_column(AgentRuns::UserContext)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum AgentRuns {
    Table,
    Phase,
    Analysis,
    UserContext,
}
