// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add node_id to deployment_containers (nullable — NULL = local node)
        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentContainers::Table)
                    .add_column(
                        ColumnDef::new(DeploymentContainers::NodeId)
                            .integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_deployment_containers_node")
                    .from(DeploymentContainers::Table, DeploymentContainers::NodeId)
                    .to(Nodes::Table, Nodes::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_containers_node_id")
                    .table(DeploymentContainers::Table)
                    .col(DeploymentContainers::NodeId)
                    .to_owned(),
            )
            .await?;

        // Add node_id to external_services (nullable — NULL = local node)
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(ColumnDef::new(ExternalServices::NodeId).integer().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_external_services_node")
                    .from(ExternalServices::Table, ExternalServices::NodeId)
                    .to(Nodes::Table, Nodes::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_external_services_node_id")
                    .table(ExternalServices::Table)
                    .col(ExternalServices::NodeId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop external_services node_id
        manager
            .drop_index(
                Index::drop()
                    .name("idx_external_services_node_id")
                    .table(ExternalServices::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_external_services_node")
                    .table(ExternalServices::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .drop_column(ExternalServices::NodeId)
                    .to_owned(),
            )
            .await?;

        // Drop deployment_containers node_id
        manager
            .drop_index(
                Index::drop()
                    .name("idx_deployment_containers_node_id")
                    .table(DeploymentContainers::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_deployment_containers_node")
                    .table(DeploymentContainers::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(DeploymentContainers::Table)
                    .drop_column(DeploymentContainers::NodeId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum DeploymentContainers {
    Table,
    NodeId,
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    NodeId,
}
