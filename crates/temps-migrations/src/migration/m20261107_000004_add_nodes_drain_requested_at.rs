// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adds `nodes.drain_requested_at`, so a draining node can be marked `offline`
//! when it stops heartbeating and still go back to `draining` when it recovers,
//! instead of taking new work. Nodes draining right now are backfilled, so their
//! drains survive this upgrade. `NULL` means the node isn't being drained.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE nodes ADD COLUMN IF NOT EXISTS drain_requested_at TIMESTAMPTZ;
UPDATE nodes SET drain_requested_at = now()
WHERE status = 'draining' AND drain_requested_at IS NULL;
"#;

const DOWN: &str = "ALTER TABLE nodes DROP COLUMN IF EXISTS drain_requested_at;";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
