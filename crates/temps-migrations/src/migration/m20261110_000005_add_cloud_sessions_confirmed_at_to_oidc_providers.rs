// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adds `cloud_sessions_confirmed_at` to `oidc_providers` (ADR-094).
//!
//! The Temps Cloud console sessions of an instance end once Cloud has not
//! confirmed them for `CLOUD_SESSION_STALE_AFTER`. The time of the last
//! confirmation was only kept in memory, so every restart during a Cloud
//! outage started the bound again. It is now kept on the Cloud-managed
//! provider's row, the one whose sessions it bounds, so a restart reads it.
//!
//! Nullable, no backfill: `NULL` means no confirmation is on record (every
//! existing row, and any provider other than the Cloud-managed one). The
//! checker treats that as unconfirmed, and its first answered check records
//! one. Rows of ordinary providers are never written.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE oidc_providers \
                 ADD COLUMN IF NOT EXISTS cloud_sessions_confirmed_at timestamptz",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE oidc_providers DROP COLUMN IF EXISTS cloud_sessions_confirmed_at",
            )
            .await?;
        Ok(())
    }
}
