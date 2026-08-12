//! ADR-038 Phase 2, milestone 2: add `cli_session_id` to `ai_conversations`.
//!
//! Stores the Claude CLI session UUID returned by the `system/init` or
//! `result` event when a conversation was driven via the interactive
//! `--permission-prompt-tool stdio` protocol.  Used to resume the session
//! with `claude --resume <session_id>` in future turns.
//!
//! `NULL` for all existing conversations and for conversations backed by
//! other providers (Codex, OpenCode, BYOK gateway) — they never emit a
//! CLI session id.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE ai_conversations \
             ADD COLUMN IF NOT EXISTS cli_session_id VARCHAR(128)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE ai_conversations \
             DROP COLUMN IF EXISTS cli_session_id",
        )
        .await?;
        Ok(())
    }
}
