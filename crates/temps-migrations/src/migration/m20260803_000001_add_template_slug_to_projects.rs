//! Persist curated-template provenance on projects.
//!
//! The value is the public slug from the bundled template catalog. It lets
//! deployment telemetry distinguish template activation from user projects
//! without sending repository names, URLs, or other user-controlled text.

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
                        ColumnDef::new(Projects::TemplateSlug)
                            .string_len(255)
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
                    .drop_column(Projects::TemplateSlug)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Projects {
    Table,
    TemplateSlug,
}
