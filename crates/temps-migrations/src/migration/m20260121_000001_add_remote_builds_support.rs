//! Migration to add remote builds support
//!
//! This migration adds:
//! - source_type column to projects table (git, docker_image, static_files)
//! - external_images table for tracking external Docker image references
//! - static_bundles table for tracking uploaded static file bundles

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add source_type column to projects table
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::SourceType)
                            .string_len(20)
                            .not_null()
                            .default("git"),
                    )
                    .to_owned(),
            )
            .await?;

        // Create external_images table
        manager
            .create_table(
                Table::create()
                    .table(ExternalImages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ExternalImages::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ExternalImages::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalImages::ImageRef)
                            .string_len(500)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExternalImages::Digest)
                            .string_len(100)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ExternalImages::SizeBytes)
                            .big_integer()
                            .null(),
                    )
                    .col(ColumnDef::new(ExternalImages::Tag).string_len(100).null())
                    .col(
                        ColumnDef::new(ExternalImages::Metadata)
                            .json_binary()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ExternalImages::PushedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ExternalImages::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Add foreign key for external_images -> projects
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_external_images_project")
                    .from(ExternalImages::Table, ExternalImages::ProjectId)
                    .to(Projects::Table, Projects::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // Create index on project_id for external_images
        manager
            .create_index(
                Index::create()
                    .name("idx_external_images_project_id")
                    .table(ExternalImages::Table)
                    .col(ExternalImages::ProjectId)
                    .to_owned(),
            )
            .await?;

        // Create unique index on project_id + image_ref
        manager
            .create_index(
                Index::create()
                    .name("idx_external_images_project_image_ref")
                    .table(ExternalImages::Table)
                    .col(ExternalImages::ProjectId)
                    .col(ExternalImages::ImageRef)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Create static_bundles table
        manager
            .create_table(
                Table::create()
                    .table(StaticBundles::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(StaticBundles::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(StaticBundles::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(StaticBundles::BlobPath)
                            .string_len(500)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(StaticBundles::OriginalFilename)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(StaticBundles::ContentType)
                            .string_len(100)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(StaticBundles::SizeBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(StaticBundles::Checksum)
                            .string_len(64)
                            .null(),
                    )
                    .col(ColumnDef::new(StaticBundles::Metadata).json_binary().null())
                    .col(
                        ColumnDef::new(StaticBundles::UploadedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(StaticBundles::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Add foreign key for static_bundles -> projects
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_static_bundles_project")
                    .from(StaticBundles::Table, StaticBundles::ProjectId)
                    .to(Projects::Table, Projects::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // Create index on project_id for static_bundles
        manager
            .create_index(
                Index::create()
                    .name("idx_static_bundles_project_id")
                    .table(StaticBundles::Table)
                    .col(StaticBundles::ProjectId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop static_bundles table
        manager
            .drop_index(
                Index::drop()
                    .name("idx_static_bundles_project_id")
                    .table(StaticBundles::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_static_bundles_project")
                    .table(StaticBundles::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(StaticBundles::Table).to_owned())
            .await?;

        // Drop external_images table
        manager
            .drop_index(
                Index::drop()
                    .name("idx_external_images_project_image_ref")
                    .table(ExternalImages::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_external_images_project_id")
                    .table(ExternalImages::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_external_images_project")
                    .table(ExternalImages::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(ExternalImages::Table).to_owned())
            .await?;

        // Drop source_type column from projects
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::SourceType)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
    SourceType,
}

#[derive(DeriveIden)]
enum ExternalImages {
    Table,
    Id,
    ProjectId,
    ImageRef,
    Digest,
    SizeBytes,
    Tag,
    Metadata,
    PushedAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum StaticBundles {
    Table,
    Id,
    ProjectId,
    BlobPath,
    OriginalFilename,
    ContentType,
    SizeBytes,
    Checksum,
    Metadata,
    UploadedAt,
    CreatedAt,
}
