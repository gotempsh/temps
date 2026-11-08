// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("ALTER TABLE external_plugin_actors ADD COLUMN IF NOT EXISTS ai_tools_enabled BOOLEAN NOT NULL DEFAULT FALSE").await?;
        manager.get_connection().execute_unprepared("ALTER TABLE ai_pending_actions ADD COLUMN IF NOT EXISTS plugin_revision TEXT").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("ALTER TABLE ai_pending_actions DROP COLUMN IF EXISTS plugin_revision").await?;
        manager.get_connection().execute_unprepared("ALTER TABLE external_plugin_actors DROP COLUMN IF EXISTS ai_tools_enabled").await?;
        Ok(())
    }
}
