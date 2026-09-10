// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Nodes::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Nodes::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Nodes::Name)
                            .string_len(100)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Nodes::TokenHash).string_len(64).not_null())
                    .col(ColumnDef::new(Nodes::TokenEncrypted).text().null())
                    .col(ColumnDef::new(Nodes::Address).string_len(255).not_null())
                    .col(
                        ColumnDef::new(Nodes::PrivateAddress)
                            .string_len(45)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Nodes::PublicEndpoint).string_len(255).null())
                    .col(ColumnDef::new(Nodes::WgPublicKey).string_len(44).null())
                    .col(
                        ColumnDef::new(Nodes::Role)
                            .string_len(20)
                            .not_null()
                            .default("worker"),
                    )
                    .col(
                        ColumnDef::new(Nodes::Status)
                            .string_len(20)
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(Nodes::Labels)
                            .json_binary()
                            .not_null()
                            .default(Expr::cust("'{}'::jsonb")),
                    )
                    .col(
                        ColumnDef::new(Nodes::Capacity)
                            .json_binary()
                            .not_null()
                            .default(Expr::cust("'{}'::jsonb")),
                    )
                    .col(
                        ColumnDef::new(Nodes::LastHeartbeat)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Nodes::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Nodes::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Index on status for filtering active nodes
        manager
            .create_index(
                Index::create()
                    .name("idx_nodes_status")
                    .table(Nodes::Table)
                    .col(Nodes::Status)
                    .to_owned(),
            )
            .await?;

        // Index on last_heartbeat for health checking
        manager
            .create_index(
                Index::create()
                    .name("idx_nodes_last_heartbeat")
                    .table(Nodes::Table)
                    .col(Nodes::LastHeartbeat)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_nodes_last_heartbeat")
                    .table(Nodes::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_nodes_status")
                    .table(Nodes::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(Nodes::Table).to_owned())
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    Id,
    Name,
    TokenHash,
    TokenEncrypted,
    Address,
    PrivateAddress,
    PublicEndpoint,
    WgPublicKey,
    Role,
    Status,
    Labels,
    Capacity,
    LastHeartbeat,
    CreatedAt,
    UpdatedAt,
}
