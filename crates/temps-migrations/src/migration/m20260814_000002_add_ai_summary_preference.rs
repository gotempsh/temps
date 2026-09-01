// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Add an instance-wide routing profile for server-authored AI summaries.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_gateway_config \
                 ADD COLUMN IF NOT EXISTS summary_provider_id VARCHAR(255), \
                 ADD COLUMN IF NOT EXISTS summary_model VARCHAR(255), \
                 ADD COLUMN IF NOT EXISTS summary_thinking_level VARCHAR(32)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_gateway_config \
                 DROP COLUMN IF EXISTS summary_thinking_level, \
                 DROP COLUMN IF EXISTS summary_model, \
                 DROP COLUMN IF EXISTS summary_provider_id",
            )
            .await?;
        Ok(())
    }
}
