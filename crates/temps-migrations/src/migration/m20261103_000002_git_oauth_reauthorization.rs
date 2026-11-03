// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bind reauthorization to an existing connection in server-side, single-use
//! state. Existing authorization states remain new-connection flows. Deleting a
//! connection invalidates its pending states instead of converting them to creates.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE oauth_states ADD COLUMN IF NOT EXISTS connection_id integer REFERENCES git_provider_connections(id) ON DELETE CASCADE, ADD COLUMN IF NOT EXISTS callback_base_url text"
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Discard pending reauthorizations before removing their target binding.
        manager.get_connection().execute_unprepared(
            "DELETE FROM oauth_states WHERE connection_id IS NOT NULL; ALTER TABLE oauth_states DROP COLUMN IF EXISTS connection_id, DROP COLUMN IF EXISTS callback_base_url"
        ).await?;
        Ok(())
    }
}
