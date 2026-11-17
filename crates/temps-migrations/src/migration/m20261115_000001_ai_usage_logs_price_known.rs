// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record whether an AI gateway usage row's cost came from a known price.
//!
//! A cost of zero is ambiguous: the model may be free (a catalogue that
//! publishes a zero price), or no price was known (a self-hosted model the
//! price lists do not cover, or a row logged before costs were estimated).
//! Usage reports tell the two apart so a free request does not make a total
//! read as partial, and an unpriced one does not read as free.
//!
//! The column is nullable with no default, so adding it does not rewrite the
//! table and needs no backfill. Rows written before it, or by an earlier binary
//! after a rollback, stay NULL and are read as priced exactly when their cost
//! is positive, which is what reports assumed before.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_usage_logs ADD COLUMN IF NOT EXISTS price_known BOOLEAN",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE ai_usage_logs DROP COLUMN IF EXISTS price_known")
            .await?;
        Ok(())
    }
}
