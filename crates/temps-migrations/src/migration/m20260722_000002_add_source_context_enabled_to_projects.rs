//! Opt-in toggle for the native source-context feature.
//!
//! Adds `projects.error_source_context_enabled` (default false). When enabled,
//! Temps accepts raw source-file uploads for the project and resolves native
//! (Go/Rust/etc.) stack frames against them at symbolication time. Off by
//! default so uploading application source is always a deliberate choice.

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
                        ColumnDef::new(Projects::ErrorSourceContextEnabled)
                            .boolean()
                            .not_null()
                            .default(false),
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
                    .drop_column(Projects::ErrorSourceContextEnabled)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    ErrorSourceContextEnabled,
}
