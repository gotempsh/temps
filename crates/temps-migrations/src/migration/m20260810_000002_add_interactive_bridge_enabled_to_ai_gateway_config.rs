//! ADR-038 Phase 2, milestone 4+: add `interactive_bridge_enabled` to
//! `ai_gateway_config`.
//!
//! When `true` (and the active agent-CLI provider is `claude_cli`), the
//! `ConversationService` uses the long-lived `--permission-prompt-tool stdio`
//! subprocess path instead of the one-shot `--dangerously-skip-permissions` path,
//! enabling mid-turn interactive tool bridging (Bash approval, `AskUserQuestion`,
//! `ExitPlanMode`).
//!
//! Defaults to `false` (opt-in) so existing deployments are unaffected.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE ai_gateway_config \
             ADD COLUMN IF NOT EXISTS interactive_bridge_enabled BOOLEAN NOT NULL DEFAULT FALSE",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE ai_gateway_config \
             DROP COLUMN IF EXISTS interactive_bridge_enabled",
        )
        .await?;
        Ok(())
    }
}
