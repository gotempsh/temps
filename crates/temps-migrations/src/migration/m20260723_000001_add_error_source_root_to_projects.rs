//! Configurable source root for error-tracking native source capture.
//!
//! Adds `projects.error_source_root` (nullable). When set, the auto-capture job
//! walks this path (relative to the git checkout) instead of the default. When
//! null, the capture defaults to the deployment's Docker build context — the
//! exact directory the image was built from — which is the correct root for
//! Dockerfile deploys (and monorepos) because frame paths are reported relative
//! to what was copied into the build.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .add_column(
                        ColumnDef::new(Projects::ErrorSourceRoot)
                            .string_len(1024)
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Projects::Table)
                    .drop_column(Projects::ErrorSourceRoot)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    ErrorSourceRoot,
}
