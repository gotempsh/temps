// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add `health_metadata` JSONB column to `external_services`.
//!
//! Engine-agnostic bag for current-state health signals that don't fit the
//! existing scalar columns (`health_status`, `last_health_error`). First
//! consumer is the Postgres WAL/archive probe, which writes under the
//! `postgres_wal` key. Other engines can add sibling keys later (e.g.,
//! `redis_memory`, `mongo_oplog`, `s3_bucket_usage`) without further
//! migration churn.
//!
//! Shape:
//! ```json
//! {
//!   "postgres_wal": { ...PostgresWalHealth... }
//! }
//! ```
//!
//! NULL means no probe has populated any signal yet.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ExternalServices::Table)
                    .add_column(
                        ColumnDef::new(ExternalServices::HealthMetadata)
                            .json_binary()
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
                    .table(ExternalServices::Table)
                    .drop_column(ExternalServices::HealthMetadata)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ExternalServices {
    Table,
    HealthMetadata,
}
