// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Make agent-run sandboxes first-class `sandboxes` rows.
//!
//! Agent runs (autofixer, workflow agents) historically created their
//! containers through the raw `SandboxProvider` without a `sandboxes` row,
//! so they were invisible to the standalone sandbox API. This migration:
//!
//! - `sandboxes.user_id` becomes nullable: agent-run sandboxes triggered
//!   by webhooks have no owning user.
//! - `sandboxes.agent_run_id` (nullable, indexed): links a sandbox row to
//!   the `agent_runs` row it executes. NULL for standalone API sandboxes.
//! - `agent_runs.triggered_by_user_id` (nullable): the authenticated user
//!   who started the run (e.g. clicked "Analyze" in the autofixer UI), so
//!   the resulting sandbox can be attributed to them in the sandbox list.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("ALTER TABLE sandboxes ALTER COLUMN user_id DROP NOT NULL")
            .await?;
        conn.execute_unprepared(
            "ALTER TABLE sandboxes ADD COLUMN IF NOT EXISTS agent_run_id INTEGER",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_sandboxes_agent_run_id ON sandboxes (agent_run_id) WHERE agent_run_id IS NOT NULL",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS triggered_by_user_id INTEGER",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE agent_runs DROP COLUMN IF EXISTS triggered_by_user_id",
        )
        .await?;
        conn.execute_unprepared("DROP INDEX IF EXISTS idx_sandboxes_agent_run_id")
            .await?;
        conn.execute_unprepared("ALTER TABLE sandboxes DROP COLUMN IF EXISTS agent_run_id")
            .await?;
        // Backfill NULL owners before restoring NOT NULL is impossible in a
        // generic down; delete orphaned agent rows instead (they only exist
        // when the up migration's feature was in use).
        conn.execute_unprepared("DELETE FROM sandboxes WHERE user_id IS NULL")
            .await?;
        conn.execute_unprepared("ALTER TABLE sandboxes ALTER COLUMN user_id SET NOT NULL")
            .await?;
        Ok(())
    }
}
