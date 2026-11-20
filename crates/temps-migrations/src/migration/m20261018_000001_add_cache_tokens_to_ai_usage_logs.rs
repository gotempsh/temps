// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record how much of a request's prompt the provider served from its cache.
//!
//! `input_tokens` keeps meaning "the whole prompt", cached or not, so context
//! readouts stay correct. These two columns say how much of that prompt was
//! read from, or written to, the provider's prompt cache, which are billed at
//! different rates than fresh input. Rows logged before this migration read as
//! zero, which prices them exactly as they were priced when they were written.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(AiUsageLogs::CacheReadTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(AiUsageLogs::CacheWriteTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AiUsageLogs::Table)
                    .drop_column(AiUsageLogs::CacheReadTokens)
                    .drop_column(AiUsageLogs::CacheWriteTokens)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AiUsageLogs {
    Table,
    CacheReadTokens,
    CacheWriteTokens,
}
