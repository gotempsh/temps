// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sea-ORM migration for the edge SQLite database.
//!
//! Runs automatically on startup — creates the edge_requests table
//! with indexes for efficient time-range + domain queries.

use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(M20260326CreateEdgeRequests)]
    }
}

#[derive(DeriveMigrationName)]
struct M20260326CreateEdgeRequests;

#[async_trait::async_trait]
impl MigrationTrait for M20260326CreateEdgeRequests {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(EdgeRequests::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EdgeRequests::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EdgeRequests::Timestamp)
                            .string_len(30)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EdgeRequests::Domain)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EdgeRequests::Path)
                            .string_len(2048)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EdgeRequests::Method)
                            .string_len(10)
                            .not_null(),
                    )
                    .col(ColumnDef::new(EdgeRequests::Status).integer().not_null())
                    .col(
                        ColumnDef::new(EdgeRequests::CacheStatus)
                            .string_len(10)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EdgeRequests::BytesSent)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(EdgeRequests::OriginLatencyMs)
                            .double()
                            .not_null()
                            .default(0.0),
                    )
                    .col(ColumnDef::new(EdgeRequests::Region).string_len(50).null())
                    .col(
                        ColumnDef::new(EdgeRequests::IsImmutable)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Indexes for efficient queries
        manager
            .create_index(
                Index::create()
                    .name("idx_edge_requests_timestamp")
                    .table(EdgeRequests::Table)
                    .col(EdgeRequests::Timestamp)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_edge_requests_domain_timestamp")
                    .table(EdgeRequests::Table)
                    .col(EdgeRequests::Domain)
                    .col(EdgeRequests::Timestamp)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_edge_requests_cache_status")
                    .table(EdgeRequests::Table)
                    .col(EdgeRequests::CacheStatus)
                    .col(EdgeRequests::Timestamp)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EdgeRequests::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum EdgeRequests {
    Table,
    Id,
    Timestamp,
    Domain,
    Path,
    Method,
    Status,
    CacheStatus,
    BytesSent,
    OriginLatencyMs,
    Region,
    IsImmutable,
}
