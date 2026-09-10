// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add protected column to environments table.
        // When true, git pushes will NOT auto-deploy to this environment.
        // Deployments must be promoted from another environment.
        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .add_column(
                        ColumnDef::new(Environments::Protected)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Add promoted_from_deployment_id to deployments table.
        // Links a promoted deployment back to its source deployment.
        manager
            .alter_table(
                Table::alter()
                    .table(Deployments::Table)
                    .add_column(
                        ColumnDef::new(Deployments::PromotedFromDeploymentId)
                            .integer()
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
                    .table(Deployments::Table)
                    .drop_column(Deployments::PromotedFromDeploymentId)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Environments::Table)
                    .drop_column(Environments::Protected)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    Protected,
}

#[derive(DeriveIden)]
enum Deployments {
    Table,
    PromotedFromDeploymentId,
}
