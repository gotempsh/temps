// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist why a git connection's last repository sync failed.
//!
//! Syncs run detached from the request that starts them, and a failure used
//! to be written only to the server log before the `syncing` flag was reset.
//! From the console, a sync that failed was indistinguishable from one that
//! succeeded and found nothing. These columns carry the outcome to the API:
//! set when a sync fails or hits its deadline, cleared when one succeeds.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE git_provider_connections \
             ADD COLUMN IF NOT EXISTS last_sync_error TEXT, \
             ADD COLUMN IF NOT EXISTS last_sync_error_at TIMESTAMPTZ",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE git_provider_connections \
             DROP COLUMN IF EXISTS last_sync_error_at, \
             DROP COLUMN IF EXISTS last_sync_error",
        )
        .await?;
        Ok(())
    }
}
