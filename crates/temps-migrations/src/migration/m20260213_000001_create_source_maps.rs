// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to add source maps support for error tracking
//!
//! This migration adds:
//! - source_maps table for storing source map artifacts tied to project releases

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create source_maps table
        manager
            .create_table(
                Table::create()
                    .table(SourceMaps::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SourceMaps::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SourceMaps::ProjectId).integer().not_null())
                    .col(
                        ColumnDef::new(SourceMaps::Release)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceMaps::FilePath)
                            .string_len(1024)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceMaps::SourceMapData)
                            .binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SourceMaps::Dist).string_len(255).null())
                    .col(
                        ColumnDef::new(SourceMaps::SizeBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SourceMaps::Checksum).string_len(64).null())
                    .col(
                        ColumnDef::new(SourceMaps::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Foreign key: source_maps -> projects
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_source_maps_project")
                    .from(SourceMaps::Table, SourceMaps::ProjectId)
                    .to(Projects::Table, Projects::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // Lookup index: (project_id, release, file_path) - unique constraint
        // This is the primary lookup path during symbolication
        manager
            .create_index(
                Index::create()
                    .name("idx_source_maps_project_release_file")
                    .table(SourceMaps::Table)
                    .col(SourceMaps::ProjectId)
                    .col(SourceMaps::Release)
                    .col(SourceMaps::FilePath)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Index for listing/deleting all maps for a release
        manager
            .create_index(
                Index::create()
                    .name("idx_source_maps_project_release")
                    .table(SourceMaps::Table)
                    .col(SourceMaps::ProjectId)
                    .col(SourceMaps::Release)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_source_maps_project_release")
                    .table(SourceMaps::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_source_maps_project_release_file")
                    .table(SourceMaps::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_source_maps_project")
                    .table(SourceMaps::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(SourceMaps::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum SourceMaps {
    Table,
    Id,
    ProjectId,
    Release,
    FilePath,
    SourceMapData,
    Dist,
    SizeBytes,
    Checksum,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}
