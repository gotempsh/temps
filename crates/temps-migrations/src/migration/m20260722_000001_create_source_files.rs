//! Migration to add raw source-file storage for error-tracking source context.
//!
//! Source maps (the `source_maps` table) only cover JavaScript/TypeScript: the
//! original source travels inside the `.map` file (`sourcesContent`). Native
//! languages (Go, Rust, Python, etc.) emit stack frames that already carry the
//! *original* filename+lineno, but the source text itself is not shipped with
//! the compiled binary. This table stores the raw source, keyed the same way
//! source maps are (`project_id, release, file_path`), so the existing
//! symbolication/rendering pipeline can attach `context_line`/`pre_context`/
//! `post_context` to native frames exactly as it does for minified JS.
//!
//! The feature is opt-in per project (see the companion migration that adds
//! `projects.error_source_context_enabled`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SourceFiles::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SourceFiles::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SourceFiles::ProjectId).integer().not_null())
                    .col(
                        ColumnDef::new(SourceFiles::Release)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceFiles::FilePath)
                            .string_len(1024)
                            .not_null(),
                    )
                    .col(ColumnDef::new(SourceFiles::Content).binary().not_null())
                    .col(
                        ColumnDef::new(SourceFiles::SizeBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SourceFiles::Checksum).string_len(64).null())
                    .col(
                        ColumnDef::new(SourceFiles::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Foreign key: source_files -> projects
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_source_files_project")
                    .from(SourceFiles::Table, SourceFiles::ProjectId)
                    .to(Projects::Table, Projects::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;

        // Primary lookup path during native-frame resolution: unique per file.
        manager
            .create_index(
                Index::create()
                    .name("idx_source_files_project_release_file")
                    .table(SourceFiles::Table)
                    .col(SourceFiles::ProjectId)
                    .col(SourceFiles::Release)
                    .col(SourceFiles::FilePath)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Index for listing/deleting all files for a release.
        manager
            .create_index(
                Index::create()
                    .name("idx_source_files_project_release")
                    .table(SourceFiles::Table)
                    .col(SourceFiles::ProjectId)
                    .col(SourceFiles::Release)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_source_files_project_release")
                    .table(SourceFiles::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_source_files_project_release_file")
                    .table(SourceFiles::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .name("fk_source_files_project")
                    .table(SourceFiles::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(SourceFiles::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum SourceFiles {
    Table,
    Id,
    ProjectId,
    Release,
    FilePath,
    Content,
    SizeBytes,
    Checksum,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}
