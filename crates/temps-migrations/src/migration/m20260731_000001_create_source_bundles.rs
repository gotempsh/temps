use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SourceBundles::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SourceBundles::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::ProjectId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::ArchivePath)
                            .string_len(500)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::OriginalFilename)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::ContentType)
                            .string_len(100)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::SizeBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::Checksum)
                            .string_len(80)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::Directory)
                            .string_len(500)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::Preset)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(ColumnDef::new(SourceBundles::Metadata).json_binary().null())
                    .col(
                        ColumnDef::new(SourceBundles::UploadedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(SourceBundles::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_source_bundles_project")
                            .from(SourceBundles::Table, SourceBundles::ProjectId)
                            .to(Projects::Table, Projects::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_source_bundles_project_uploaded")
                    .if_not_exists()
                    .table(SourceBundles::Table)
                    .col(SourceBundles::ProjectId)
                    .col(SourceBundles::UploadedAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(SourceBundles::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum SourceBundles {
    Table,
    Id,
    ProjectId,
    ArchivePath,
    OriginalFilename,
    ContentType,
    SizeBytes,
    Checksum,
    Directory,
    Preset,
    Metadata,
    UploadedAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    Id,
}
