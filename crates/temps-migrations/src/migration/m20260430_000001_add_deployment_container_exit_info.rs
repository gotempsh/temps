// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

/// Capture WHY a container exited so the UI can show "OOMKilled" or
/// "Exited(137) — SIGKILL" instead of a bare "Exited" pill. All columns are
/// nullable: existing rows and currently-running containers don't have an
/// exit yet, and Docker may not surface every field for every shutdown.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentContainers::Table)
                    .add_column(
                        ColumnDef::new(DeploymentContainers::ExitCode)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(DeploymentContainers::ExitReason)
                            .string_len(255)
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(DeploymentContainers::OomKilled)
                            .boolean()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(DeploymentContainers::ErrorMessage)
                            .text()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(DeploymentContainers::FinishedAt)
                            .timestamp_with_time_zone()
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
                    .table(DeploymentContainers::Table)
                    .drop_column(DeploymentContainers::ExitCode)
                    .drop_column(DeploymentContainers::ExitReason)
                    .drop_column(DeploymentContainers::OomKilled)
                    .drop_column(DeploymentContainers::ErrorMessage)
                    .drop_column(DeploymentContainers::FinishedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum DeploymentContainers {
    Table,
    ExitCode,
    ExitReason,
    OomKilled,
    ErrorMessage,
    FinishedAt,
}
