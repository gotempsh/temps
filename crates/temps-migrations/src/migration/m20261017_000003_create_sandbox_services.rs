// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which services a sandbox is linked to (ADR-051), the sandbox counterpart of
//! `project_services`.
//!
//! A row says "this sandbox may use this service with credentials of its own".
//! The credentials are never stored: they are derived or issued from the
//! service when a command runs. `project_id` records the project the link was
//! made through, so the sandbox's database and bucket keep the project's name
//! and a new chat on the project continues with the same data; it is empty for
//! a link made to a service directly. Rows go away with the sandbox or the
//! service.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SandboxServices::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SandboxServices::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SandboxServices::SandboxId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxServices::ServiceId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SandboxServices::ProjectId).integer())
                    .col(ColumnDef::new(SandboxServices::CreatedBy).integer())
                    .col(
                        ColumnDef::new(SandboxServices::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_services_sandbox")
                            .from(SandboxServices::Table, SandboxServices::SandboxId)
                            .to(Sandboxes::Table, Sandboxes::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_services_service")
                            .from(SandboxServices::Table, SandboxServices::ServiceId)
                            .to(ExternalServices::Table, ExternalServices::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_services_project")
                            .from(SandboxServices::Table, SandboxServices::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_sandbox_services_sandbox_service")
                    .table(SandboxServices::Table)
                    .col(SandboxServices::SandboxId)
                    .col(SandboxServices::ServiceId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        // The reverse lookup: the sandboxes using a service, when it changes.
        manager
            .create_index(
                Index::create()
                    .name("idx_sandbox_services_service")
                    .table(SandboxServices::Table)
                    .col(SandboxServices::ServiceId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SandboxServices::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum SandboxServices {
    Table,
    Id,
    SandboxId,
    ServiceId,
    ProjectId,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Sandboxes {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}
