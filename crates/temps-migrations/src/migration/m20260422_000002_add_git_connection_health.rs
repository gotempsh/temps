// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(GitProviderConnections::Table)
                    .add_column(
                        ColumnDef::new(GitProviderConnections::HealthStatus)
                            .string_len(16)
                            .not_null()
                            .default("unknown"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(GitProviderConnections::Table)
                    .add_column(
                        ColumnDef::new(GitProviderConnections::HealthMessage)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(GitProviderConnections::Table)
                    .add_column(
                        ColumnDef::new(GitProviderConnections::LastHealthCheckAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(GitProviderConnections::Table)
                    .add_column(
                        ColumnDef::new(GitProviderConnections::ConsecutiveHealthFailures)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for col in [
            GitProviderConnections::HealthStatus,
            GitProviderConnections::HealthMessage,
            GitProviderConnections::LastHealthCheckAt,
            GitProviderConnections::ConsecutiveHealthFailures,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(GitProviderConnections::Table)
                        .drop_column(col)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum GitProviderConnections {
    Table,
    HealthStatus,
    HealthMessage,
    LastHealthCheckAt,
    ConsecutiveHealthFailures,
}
