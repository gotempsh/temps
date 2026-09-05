// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Makes an AI turn a server-owned resource rather than browser-owned state.
//!
//! A conversation can have at most one active turn. The opaque turn id makes
//! message submission idempotent and the persisted status lets a refreshed UI
//! reattach without pretending the harness stopped when only the SSE viewer
//! disconnected.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_conversations
                   ADD COLUMN IF NOT EXISTS turn_status TEXT NOT NULL DEFAULT 'idle',
                   ADD COLUMN IF NOT EXISTS active_turn_id TEXT,
                   ADD COLUMN IF NOT EXISTS last_turn_id TEXT,
                   ADD COLUMN IF NOT EXISTS turn_started_at TIMESTAMPTZ;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE ai_conversations
                   DROP COLUMN IF EXISTS turn_started_at,
                   DROP COLUMN IF EXISTS active_turn_id,
                   DROP COLUMN IF EXISTS last_turn_id,
                   DROP COLUMN IF EXISTS turn_status;",
            )
            .await?;
        Ok(())
    }
}
