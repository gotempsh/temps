// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Make application topology non-empty and persist desired workspace state.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiApplicationProjects::Table)
                    .add_column(
                        ColumnDef::new(AiApplicationProjects::IsPrimary)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Existing applications predate the non-empty invariant. Mark their
        // first link primary; empty legacy applications remain readable but
        // cannot create threads until a project is linked.
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE ai_application_projects AS link SET is_primary = TRUE \
                 WHERE link.id = (SELECT first_link.id FROM ai_application_projects AS first_link \
                 WHERE first_link.application_id = link.application_id ORDER BY first_link.id LIMIT 1); \
                 CREATE UNIQUE INDEX uq_ai_application_primary_project \
                 ON ai_application_projects (application_id) WHERE is_primary = TRUE;",
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AiApplicationWorkspaces::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::ApplicationId)
                            .big_integer()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(AiApplicationWorkspaces::SandboxPublicId).string_len(64))
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::DesiredState)
                            .string_len(32)
                            .not_null()
                            .default("running"),
                    )
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::Runtime)
                            .string_len(32)
                            .not_null()
                            .default("node"),
                    )
                    .col(ColumnDef::new(AiApplicationWorkspaces::Image).string_len(512))
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::CpuLimit)
                            .double()
                            .not_null()
                            .default(4.0),
                    )
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::MemoryLimitMb)
                            .big_integer()
                            .not_null()
                            .default(8192),
                    )
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::PidsLimit)
                            .big_integer()
                            .not_null()
                            .default(512),
                    )
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::DiskLimitMb)
                            .big_integer()
                            .not_null()
                            .default(10240),
                    )
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::IdleTimeoutSecs)
                            .big_integer()
                            .not_null()
                            .default(900),
                    )
                    .col(ColumnDef::new(AiApplicationWorkspaces::LastError).text())
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(AiApplicationWorkspaces::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ai_application_workspaces_application")
                            .from(
                                AiApplicationWorkspaces::Table,
                                AiApplicationWorkspaces::ApplicationId,
                            )
                            .to(AiApplications::Table, AiApplications::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "INSERT INTO ai_application_workspaces (application_id) \
                 SELECT id FROM ai_applications ON CONFLICT (application_id) DO NOTHING;",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_application_workspaces \
                 ADD CONSTRAINT ai_application_workspaces_desired_state_check \
                   CHECK (desired_state IN ('running', 'paused')), \
                 ADD CONSTRAINT ai_application_workspaces_runtime_check \
                   CHECK (runtime IN ('node', 'bun', 'python', 'rust', 'go', 'full', 'custom')), \
                 ADD CONSTRAINT ai_application_workspaces_cpu_check \
                   CHECK (cpu_limit BETWEEN 0.25 AND 32), \
                 ADD CONSTRAINT ai_application_workspaces_memory_check \
                   CHECK (memory_limit_mb BETWEEN 256 AND 131072), \
                 ADD CONSTRAINT ai_application_workspaces_pids_check \
                   CHECK (pids_limit BETWEEN 64 AND 32768), \
                 ADD CONSTRAINT ai_application_workspaces_disk_check \
                   CHECK (disk_limit_mb BETWEEN 512 AND 1048576), \
                 ADD CONSTRAINT ai_application_workspaces_idle_timeout_check \
                   CHECK (idle_timeout_secs BETWEEN 60 AND 86400), \
                 ADD CONSTRAINT ai_application_workspaces_custom_image_check \
                   CHECK (runtime <> 'custom' OR image IS NOT NULL);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(AiApplicationWorkspaces::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS uq_ai_application_primary_project")
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(AiApplicationProjects::Table)
                    .drop_column(AiApplicationProjects::IsPrimary)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AiApplications {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum AiApplicationProjects {
    Table,
    IsPrimary,
}

#[derive(DeriveIden)]
enum AiApplicationWorkspaces {
    Table,
    Id,
    ApplicationId,
    SandboxPublicId,
    DesiredState,
    Runtime,
    Image,
    CpuLimit,
    MemoryLimitMb,
    PidsLimit,
    DiskLimitMb,
    IdleTimeoutSecs,
    LastError,
    CreatedAt,
    UpdatedAt,
}
