// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration to create the static_asset_cache table.
//!
//! Maps URL paths to CAS content hashes for stale-chunk fallback serving.
//! The proxy queries this table to resolve URL → blob hash → blob on disk.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("static_asset_cache"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("url_path")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("content_hash"))
                            .string_len(64)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("environment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("deployment_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("size_bytes"))
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Index for proxy lookups: find asset by URL path within a project
        manager
            .create_index(
                Index::create()
                    .name("idx_static_asset_cache_lookup")
                    .table(Alias::new("static_asset_cache"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("url_path"))
                    .to_owned(),
            )
            .await?;

        // Index for cleanup: delete all assets for a deployment
        manager
            .create_index(
                Index::create()
                    .name("idx_static_asset_cache_deployment")
                    .table(Alias::new("static_asset_cache"))
                    .col(Alias::new("deployment_id"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("static_asset_cache"))
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
