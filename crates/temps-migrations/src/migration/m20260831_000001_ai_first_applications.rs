// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! First-class application scopes and typed AI thread artifacts.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AiApplications::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiApplications::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiApplications::PublicId)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(AiApplications::Name)
                            .string_len(200)
                            .not_null(),
                    )
                    .col(ColumnDef::new(AiApplications::Description).text())
                    .col(
                        ColumnDef::new(AiApplications::Status)
                            .string_len(32)
                            .not_null()
                            .default("active"),
                    )
                    .col(
                        ColumnDef::new(AiApplications::CreatedBy)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplications::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiApplications::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_applications_created_by")
                            .from(AiApplications::Table, AiApplications::CreatedBy)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AiApplicationProjects::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiApplicationProjects::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationProjects::ApplicationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationProjects::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationProjects::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_application_projects_application")
                            .from(
                                AiApplicationProjects::Table,
                                AiApplicationProjects::ApplicationId,
                            )
                            .to(AiApplications::Table, AiApplications::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_application_projects_project")
                            .from(
                                AiApplicationProjects::Table,
                                AiApplicationProjects::ProjectId,
                            )
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_ai_application_projects")
                            .col(AiApplicationProjects::ApplicationId)
                            .col(AiApplicationProjects::ProjectId)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(AiConversations::Table)
                    .add_column(ColumnDef::new(AiConversations::ApplicationId).big_integer())
                    .to_owned(),
            )
            .await?;
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_ai_conversations_application")
                    .from(AiConversations::Table, AiConversations::ApplicationId)
                    .to(AiApplications::Table, AiApplications::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AiThreadArtifacts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiThreadArtifacts::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::PublicId)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::ConversationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::ApplicationId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::Kind)
                            .string_len(64)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::SchemaVersion)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(ColumnDef::new(AiThreadArtifacts::Title).string_len(200))
                    .col(
                        ColumnDef::new(AiThreadArtifacts::Payload)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::Status)
                            .string_len(32)
                            .not_null()
                            .default("active"),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::CreatedBy)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiThreadArtifacts::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_thread_artifacts_conversation")
                            .from(AiThreadArtifacts::Table, AiThreadArtifacts::ConversationId)
                            .to(AiConversations::Table, AiConversations::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_thread_artifacts_application")
                            .from(AiThreadArtifacts::Table, AiThreadArtifacts::ApplicationId)
                            .to(AiApplications::Table, AiApplications::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_thread_artifacts_created_by")
                            .from(AiThreadArtifacts::Table, AiThreadArtifacts::CreatedBy)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_ai_applications_creator_activity ON ai_applications (created_by, updated_at DESC); \
                 CREATE INDEX IF NOT EXISTS idx_ai_application_projects_project ON ai_application_projects (project_id); \
                 CREATE INDEX IF NOT EXISTS idx_ai_conversations_application_activity ON ai_conversations (application_id, last_activity_at DESC) WHERE application_id IS NOT NULL; \
                 CREATE INDEX IF NOT EXISTS idx_ai_thread_artifacts_conversation ON ai_thread_artifacts (conversation_id, created_at);",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AiThreadArtifacts::Table).to_owned())
            .await?;
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_ai_conversations_application")
                    .table(AiConversations::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(AiConversations::Table)
                    .drop_column(AiConversations::ApplicationId)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(AiApplicationProjects::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AiApplications::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiApplications {
    Table,
    Id,
    PublicId,
    Name,
    Description,
    Status,
    CreatedBy,
    CreatedAt,
    UpdatedAt,
}
#[derive(DeriveIden)]
enum AiApplicationProjects {
    Table,
    Id,
    ApplicationId,
    ProjectId,
    CreatedAt,
}
#[derive(DeriveIden)]
enum AiConversations {
    Table,
    Id,
    ApplicationId,
}
#[derive(DeriveIden)]
enum AiThreadArtifacts {
    Table,
    Id,
    PublicId,
    ConversationId,
    ApplicationId,
    Kind,
    SchemaVersion,
    Title,
    Payload,
    Status,
    CreatedBy,
    CreatedAt,
    UpdatedAt,
}
#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
