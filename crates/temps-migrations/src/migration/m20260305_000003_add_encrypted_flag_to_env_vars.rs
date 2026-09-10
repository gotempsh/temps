// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to add is_encrypted column to env_vars table
//!
//! Adds a boolean flag to track whether the value is stored encrypted.
//! Existing rows are marked as not encrypted for backward compatibility.
//! New rows will be encrypted by default.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(EnvVars::Table)
                    .add_column(
                        ColumnDef::new(EnvVars::IsEncrypted)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(EnvVars::Table)
                    .drop_column(EnvVars::IsEncrypted)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum EnvVars {
    Table,
    IsEncrypted,
}
