// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adds `feature_flags.last_evaluated_at` — when an app last actually
//! evaluated this flag.
//!
//! This is the column that makes stale-flag cleanup possible: without it, the
//! only way to know whether a flag is still referenced by running code is to
//! grep every deployed service.
//!
//! It lives on `feature_flags`, not `feature_flag_environments`, for two
//! reasons. The row always exists (a flag with no override in any environment
//! has no `feature_flag_environments` row to stamp, and creating one purely to
//! carry a timestamp would put rows with no override into the override table).
//! And the question it answers — "is it safe to delete this flag?" — is
//! answered across all environments at once: a flag still evaluated anywhere
//! is not safe to remove. Per-environment resolution arrives with full
//! exposure events, which carry the environment on the event itself.
//!
//! Fed by `POST /flags/exposure`, which the SDK calls on an interval with the
//! set of keys it actually evaluated — never per evaluation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(FeatureFlags::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(FeatureFlags::LastEvaluatedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // NULL means "never seen", which is a meaningful answer in its own
        // right — a flag created and never referenced. Existing rows are left
        // NULL rather than backfilled to `now()`, because pretending every
        // pre-existing flag was just evaluated is exactly the lie this column
        // exists to avoid.
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(FeatureFlags::Table)
                    .drop_column(FeatureFlags::LastEvaluatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum FeatureFlags {
    Table,
    LastEvaluatedAt,
}
