// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to create log aggregator tables
//!
//! Creates:
//! - `log_chunks` table for chunk metadata (the actual log data lives in
//!   compressed .ndjson.zst files on disk/S3, referenced by `storage_key`)

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── log_chunks table ────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(LogChunks::Table)
                    .col(
                        ColumnDef::new(LogChunks::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(LogChunks::ProjectId).integer().not_null())
                    .col(ColumnDef::new(LogChunks::Env).string().not_null())
                    .col(ColumnDef::new(LogChunks::Service).string().not_null())
                    .col(ColumnDef::new(LogChunks::ContainerId).string().not_null())
                    .col(ColumnDef::new(LogChunks::DeployId).uuid().null())
                    .col(
                        ColumnDef::new(LogChunks::StartedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(LogChunks::EndedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(LogChunks::StorageKey).string().not_null())
                    .col(ColumnDef::new(LogChunks::LineCount).integer().not_null())
                    .col(
                        ColumnDef::new(LogChunks::CompressedSizeBytes)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(LogChunks::HasErrors)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(LogChunks::LineOffsets)
                            .array(ColumnType::Integer)
                            .not_null()
                            .default(Expr::cust("ARRAY[]::INTEGER[]")),
                    )
                    .to_owned(),
            )
            .await?;

        // Primary lookup index: (project_id, service, started_at)
        manager
            .create_index(
                Index::create()
                    .name("idx_log_chunks_project_service_time")
                    .table(LogChunks::Table)
                    .col(LogChunks::ProjectId)
                    .col(LogChunks::Service)
                    .col(LogChunks::StartedAt)
                    .to_owned(),
            )
            .await?;

        // Deploy-based lookup
        manager
            .create_index(
                Index::create()
                    .name("idx_log_chunks_deploy_id")
                    .table(LogChunks::Table)
                    .col(LogChunks::DeployId)
                    .to_owned(),
            )
            .await?;

        // Container-based lookup (used for resume-after on restart)
        manager
            .create_index(
                Index::create()
                    .name("idx_log_chunks_container_ended")
                    .table(LogChunks::Table)
                    .col(LogChunks::ContainerId)
                    .col(LogChunks::EndedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_log_chunks_container_ended")
                    .table(LogChunks::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_log_chunks_deploy_id")
                    .table(LogChunks::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_log_chunks_project_service_time")
                    .table(LogChunks::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(LogChunks::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum LogChunks {
    Table,
    Id,
    ProjectId,
    Env,
    Service,
    ContainerId,
    DeployId,
    StartedAt,
    EndedAt,
    StorageKey,
    LineCount,
    CompressedSizeBytes,
    HasErrors,
    LineOffsets,
}
