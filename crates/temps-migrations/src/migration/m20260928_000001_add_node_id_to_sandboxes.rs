// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pin each sandbox to the node that hosts it (ADR-048).
//!
//! `node_id` NULL means the sandbox runs on the control plane — every row
//! written before this column existed, and every sandbox created without a
//! worker. The FK is `ON DELETE SET NULL` so destroyed sandbox rows never
//! block removing a node; deleting a node that still hosts live sandboxes is
//! refused by the node delete handler before the row is touched, so a live
//! sandbox is never silently re-homed to the control plane.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE sandboxes \
             ADD COLUMN IF NOT EXISTS node_id INTEGER \
             REFERENCES nodes(id) ON DELETE SET NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_sandboxes_node_id \
             ON sandboxes (node_id) WHERE node_id IS NOT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP INDEX IF EXISTS idx_sandboxes_node_id")
            .await?;
        db.execute_unprepared("ALTER TABLE sandboxes DROP COLUMN IF EXISTS node_id")
            .await?;
        Ok(())
    }
}
