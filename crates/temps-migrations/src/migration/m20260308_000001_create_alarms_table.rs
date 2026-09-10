// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alarms::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alarms::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alarms::ProjectId).integer().not_null())
                    .col(ColumnDef::new(Alarms::EnvironmentId).integer().not_null())
                    .col(ColumnDef::new(Alarms::DeploymentId).integer().not_null())
                    .col(ColumnDef::new(Alarms::ContainerId).integer().null())
                    // What kind of alarm
                    .col(ColumnDef::new(Alarms::AlarmType).string_len(50).not_null())
                    .col(ColumnDef::new(Alarms::Severity).string_len(20).not_null())
                    .col(
                        ColumnDef::new(Alarms::Status)
                            .string_len(20)
                            .not_null()
                            .default("firing"),
                    )
                    // What happened
                    .col(ColumnDef::new(Alarms::Title).string_len(255).not_null())
                    .col(ColumnDef::new(Alarms::Message).text().null())
                    .col(ColumnDef::new(Alarms::Metadata).json_binary().null())
                    // Timing
                    .col(
                        ColumnDef::new(Alarms::FiredAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alarms::AcknowledgedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Alarms::AcknowledgedBy).integer().null())
                    .col(
                        ColumnDef::new(Alarms::ResolvedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alarms::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alarms::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    // Foreign keys
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_alarms_project")
                            .from(Alarms::Table, Alarms::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_alarms_environment")
                            .from(Alarms::Table, Alarms::EnvironmentId)
                            .to(Environments::Table, Environments::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_alarms_deployment")
                            .from(Alarms::Table, Alarms::DeploymentId)
                            .to(Deployments::Table, Deployments::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_alarms_container")
                            .from(Alarms::Table, Alarms::ContainerId)
                            .to(DeploymentContainers::Table, DeploymentContainers::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_alarms_acknowledged_by")
                            .from(Alarms::Table, Alarms::AcknowledgedBy)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Indexes for efficient queries
        manager
            .create_index(
                Index::create()
                    .name("idx_alarms_project_status")
                    .table(Alarms::Table)
                    .col(Alarms::ProjectId)
                    .col(Alarms::Status)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_alarms_environment")
                    .table(Alarms::Table)
                    .col(Alarms::EnvironmentId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_alarms_deployment")
                    .table(Alarms::Table)
                    .col(Alarms::DeploymentId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_alarms_fired_at")
                    .table(Alarms::Table)
                    .col((Alarms::FiredAt, IndexOrder::Desc))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_alarms_type_status")
                    .table(Alarms::Table)
                    .col(Alarms::AlarmType)
                    .col(Alarms::Status)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alarms::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Alarms {
    Table,
    Id,
    ProjectId,
    EnvironmentId,
    DeploymentId,
    ContainerId,
    AlarmType,
    Severity,
    Status,
    Title,
    Message,
    Metadata,
    FiredAt,
    AcknowledgedAt,
    AcknowledgedBy,
    ResolvedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Environments {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Deployments {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum DeploymentContainers {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
