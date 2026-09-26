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
                    .table(AiApplicationGitBindings::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::ApplicationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::ConnectionId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::RepositoryId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::RepositoryUrl)
                            .string_len(2048)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::RemoteName)
                            .string_len(64)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiApplicationGitBindings::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_git_binding_application")
                            .from(
                                AiApplicationGitBindings::Table,
                                AiApplicationGitBindings::ApplicationId,
                            )
                            .to(AiApplications::Table, AiApplications::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_git_binding_connection")
                            .from(
                                AiApplicationGitBindings::Table,
                                AiApplicationGitBindings::ConnectionId,
                            )
                            .to(GitProviderConnections::Table, GitProviderConnections::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_git_binding_repository")
                            .from(
                                AiApplicationGitBindings::Table,
                                AiApplicationGitBindings::RepositoryId,
                            )
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_ai_git_binding_remote")
                            .col(AiApplicationGitBindings::ApplicationId)
                            .col(AiApplicationGitBindings::ProjectId)
                            .col(AiApplicationGitBindings::RemoteName)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;
        manager.get_connection().execute_unprepared(
            "ALTER TABLE ai_application_git_bindings ADD CONSTRAINT fk_ai_git_binding_application_project FOREIGN KEY (application_id, project_id) REFERENCES ai_application_projects (application_id, project_id) ON DELETE CASCADE"
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(AiApplicationGitBindings::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AiApplicationGitBindings {
    Table,
    Id,
    ApplicationId,
    ProjectId,
    ConnectionId,
    RepositoryId,
    RepositoryUrl,
    RemoteName,
    CreatedAt,
    UpdatedAt,
}
#[derive(DeriveIden)]
enum AiApplications {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum GitProviderConnections {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}
