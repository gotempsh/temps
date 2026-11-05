// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Serialize OAuth token refreshes per git connection, across processes.
//!
//! - `credential_generation` counts grant replacements (reauthorization, a new
//!   token entered by the user). A refresh writes only while the generation it
//!   started from is current, so it survives a concurrent rotation but never
//!   overwrites a completed reauthorization. Existing rows start at 0: nothing
//!   is in flight across the upgrade that this could misjudge.
//! - `token_refresh_lease_id` and `token_refresh_lease_expires_at` are a lease
//!   claimed by compare-and-set before the provider call. A row is used rather
//!   than a Postgres advisory lock so no pooled database connection is held
//!   across the network request. NULL means nobody is refreshing.
//!
//! No backfill is needed. All three columns are additive, so the previous binary
//! keeps working against this schema.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE git_provider_connections
                    ADD COLUMN IF NOT EXISTS credential_generation integer NOT NULL DEFAULT 0,
                    ADD COLUMN IF NOT EXISTS token_refresh_lease_id uuid,
                    ADD COLUMN IF NOT EXISTS token_refresh_lease_expires_at timestamptz",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE git_provider_connections
                    DROP COLUMN IF EXISTS token_refresh_lease_expires_at,
                    DROP COLUMN IF EXISTS token_refresh_lease_id,
                    DROP COLUMN IF EXISTS credential_generation",
            )
            .await?;
        Ok(())
    }
}
