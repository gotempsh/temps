// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Phase 2 of ADR-037: extend `ai_gateway_config` with the two columns that
//! let `DispatchingAiService` route per-scope to either the BYOK gateway or a
//! subscription-backed agent CLI.
//!
//! - `provider_type`: `'gateway'` (default, existing BYOK path) or `'agent_cli'`.
//! - `agent_cli_provider_id`: catalog entry id (`'claude_cli'`, `'codex_cli'`,
//!   `'opencode'`); `NULL` when `provider_type = 'gateway'`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE ai_gateway_config ADD COLUMN IF NOT EXISTS provider_type VARCHAR(32) NOT NULL DEFAULT 'gateway'",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE ai_gateway_config ADD COLUMN IF NOT EXISTS agent_cli_provider_id VARCHAR(64)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE ai_gateway_config DROP COLUMN IF EXISTS agent_cli_provider_id",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE ai_gateway_config DROP COLUMN IF EXISTS provider_type",
        )
        .await?;
        Ok(())
    }
}
