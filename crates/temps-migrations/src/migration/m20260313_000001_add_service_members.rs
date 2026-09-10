// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add topology column to external_services (default 'standalone' for backward compat)
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::Topology)
                            .string_len(20)
                            .not_null()
                            .default("standalone"),
                    )
                    .to_owned(),
            )
            .await?;

        // Create service_members table
        manager
            .create_table(
                Table::create()
                    .table(ServiceMembers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ServiceMembers::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ServiceMembers::ServiceId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ServiceMembers::NodeId).integer().null())
                    .col(
                        ColumnDef::new(ServiceMembers::Role)
                            .string_len(30)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ServiceMembers::ContainerId)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ServiceMembers::ContainerName)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ServiceMembers::Hostname)
                            .string_len(255)
                            .null(),
                    )
                    .col(ColumnDef::new(ServiceMembers::Port).integer().null())
                    .col(
                        ColumnDef::new(ServiceMembers::Status)
                            .string_len(30)
                            .not_null()
                            .default("provisioning"),
                    )
                    .col(
                        ColumnDef::new(ServiceMembers::Ordinal)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(ServiceMembers::Config).text().null())
                    .col(
                        ColumnDef::new(ServiceMembers::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ServiceMembers::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_service_members_service")
                            .from(ServiceMembers::Table, ServiceMembers::ServiceId)
                            .to(ExternalServices::Table, ExternalServices::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_service_members_node")
                            .from(ServiceMembers::Table, ServiceMembers::NodeId)
                            .to(Nodes::Table, Nodes::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Indexes
        manager
            .create_index(
                Index::create()
                    .name("idx_service_members_service_id")
                    .table(ServiceMembers::Table)
                    .col(ServiceMembers::ServiceId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_service_members_node_id")
                    .table(ServiceMembers::Table)
                    .col(ServiceMembers::NodeId)
                    .to_owned(),
            )
            .await?;

        // Unique constraint: one ordinal per service
        manager
            .create_index(
                Index::create()
                    .name("uq_service_members_service_ordinal")
                    .table(ServiceMembers::Table)
                    .col(ServiceMembers::ServiceId)
                    .col(ServiceMembers::Ordinal)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ServiceMembers::Table).to_owned())
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .drop_column(ExternalServices::Topology)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    Id,
    Topology,
}

#[derive(DeriveIden)]
enum ServiceMembers {
    Table,
    Id,
    ServiceId,
    NodeId,
    Role,
    ContainerId,
    ContainerName,
    Hostname,
    Port,
    Status,
    Ordinal,
    Config,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    Id,
}
