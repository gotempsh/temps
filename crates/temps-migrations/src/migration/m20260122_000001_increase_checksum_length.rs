//! Migration to increase checksum column length
//!
//! The checksum column in static_bundles table was too short (64 chars)
//! to store "sha256:{hash}" format (71 chars: 7 + 64)

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Increase checksum column length from 64 to 100 characters
        manager
            .alter_table(
                Table::alter()
                    .table(StaticBundles::Table)
                    .modify_column(
                        ColumnDef::new(StaticBundles::Checksum)
                            .string_len(100)
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Revert checksum column length back to 64 characters
        manager
            .alter_table(
                Table::alter()
                    .table(StaticBundles::Table)
                    .modify_column(
                        ColumnDef::new(StaticBundles::Checksum)
                            .string_len(64)
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum StaticBundles {
    Table,
    Checksum,
}
