// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Per-service opt-in for AI agent read access to *row data*.
        //
        // The AI `call_api` read tool is an allowlist of endpoints whose
        // responses contain no secrets (metrics, logs, masked metadata). Row
        // data is a different risk class entirely: a `users` table can hold
        // password hashes, API tokens and customer PII, and enabling the
        // agent to read it means shipping those rows to a third-party LLM
        // provider. That is a decision the operator must make deliberately,
        // per service — not something a platform upgrade turns on for them.
        //
        // NOT NULL DEFAULT false: every existing service stays closed on
        // upgrade, and the agent reports the service as opt-out (with the
        // console path to enable it) rather than failing opaquely.
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::AiDataAccess)
                            .boolean()
                            .not_null()
                            .default(false),
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
                    .table(ExternalServices::Table)
                    .drop_column(ExternalServices::AiDataAccess)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    AiDataAccess,
}
