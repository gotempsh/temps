// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to drop asset_manifests table.
//!
//! The asset_manifests table is no longer needed — static assets are now stored
//! in a path-keyed file store with no database involvement.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop the table if it exists (safe for fresh installs that never had it)
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("asset_manifests"))
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // No-op: we don't recreate the table on rollback
        Ok(())
    }
}
