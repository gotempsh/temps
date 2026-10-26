// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project and per-external-service overrides of the container log
//! ingestion budget (`AppSettings.container_logs.ingest_lines_per_sec` /
//! `ingest_kib_per_sec`). NULL inherits the platform budget; 0 disables
//! that limit for the project or service.

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
                        ColumnDef::new(Projects::LogIngestLinesPerSec)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(Projects::LogIngestKibPerSec)
                            .integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::LogIngestLinesPerSec)
                            .integer()
                            .null(),
                    )
                    .add_column(
                        ColumnDef::new(ExternalServices::LogIngestKibPerSec)
                            .integer()
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
                    .table(ExternalServices::Table)
                    .drop_column(ExternalServices::LogIngestLinesPerSec)
                    .drop_column(ExternalServices::LogIngestKibPerSec)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::LogIngestLinesPerSec)
                    .drop_column(Projects::LogIngestKibPerSec)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    LogIngestLinesPerSec,
    LogIngestKibPerSec,
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    LogIngestLinesPerSec,
    LogIngestKibPerSec,
}
