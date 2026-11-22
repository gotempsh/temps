// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adds `secret_suggestion_dismissed_at` to `env_vars`.
//!
//! Temps suggests storing a regular variable as a secret when its value looks
//! like a credential. The suggestion itself is computed on every read, so it
//! improves with the detector and needs no backfill; this column only records
//! the operator's answer ("this is not a credential"), which must persist.

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
                    // Idempotent: the column may exist where this migration
                    // ran under an earlier, pre-release name.
                    .add_column_if_not_exists(
                        ColumnDef::new(EnvVars::SecretSuggestionDismissedAt)
                            .timestamp_with_time_zone()
                            .null(),
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
                    .drop_column(EnvVars::SecretSuggestionDismissedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum EnvVars {
    Table,
    SecretSuggestionDismissedAt,
}
