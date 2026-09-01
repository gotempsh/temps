// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

/// Adds the runtime-state columns that complement the exit-info columns from
/// m20260430_000001. Split into its own migration because the first one
/// already shipped to some environments — re-running an `alter_table` with
/// extra columns is risky, so we add these two independently and idempotently.
///
/// `started_at` lets the UI show uptime that resets on a restart-in-place
/// (distinct from `created_at`), and `cpu_limit_cores` lets the configuration
/// tab surface the actual Docker-applied CPU cap so drift between configured
/// and observed limits is visible.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentContainers::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(DeploymentContainers::StartedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(DeploymentContainers::CpuLimitCores)
                            .double()
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
                    .drop_column(DeploymentContainers::StartedAt)
                    .drop_column(DeploymentContainers::CpuLimitCores)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum DeploymentContainers {
    Table,
    StartedAt,
    CpuLimitCores,
}
