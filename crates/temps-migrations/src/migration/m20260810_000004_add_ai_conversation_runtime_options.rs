// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persist the immutable runtime options selected for each AI conversation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_conversations \
                 ADD COLUMN IF NOT EXISTS ai_model VARCHAR(128) NOT NULL DEFAULT '', \
                 ADD COLUMN IF NOT EXISTS ai_thinking_level VARCHAR(64), \
                 ADD COLUMN IF NOT EXISTS ai_permission_mode VARCHAR(64) NOT NULL DEFAULT 'default'",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_conversations \
                 DROP COLUMN IF EXISTS ai_permission_mode, \
                 DROP COLUMN IF EXISTS ai_thinking_level, \
                 DROP COLUMN IF EXISTS ai_model",
            )
            .await?;
        Ok(())
    }
}
