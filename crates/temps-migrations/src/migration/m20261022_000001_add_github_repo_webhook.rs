// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project GitHub repository webhooks for non-App connections.
//!
//! A GitHub App delivers push events through the App's own webhook, signed
//! with the App's secret. A GitHub connection authenticated with a personal
//! access token (or OAuth) has no App, so nothing ever told Temps about a
//! push and auto-deploy silently did nothing. Temps now registers a repository
//! webhook for those projects, signed with a secret of its own; these columns
//! hold the hook ID (so it can be deleted when the repository changes) and the
//! encrypted signing secret (so deliveries can be verified).

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
                    .add_column_if_not_exists(
                        ColumnDef::new(Projects::GithubWebhookId)
                            .big_integer()
                            .null(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(Projects::GithubWebhookSigningToken)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::GithubWebhookSigningToken)
                    .drop_column(Projects::GithubWebhookId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    GithubWebhookId,
    GithubWebhookSigningToken,
}
