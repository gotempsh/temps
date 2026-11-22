// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Checking a monitor from more than one place (ADR 108 D5).
//!
//! - `probe_from_nodes`: the monitor is also checked from the cluster's
//!   worker nodes and is down only when most locations agree. Off by
//!   default, and only for monitors of targets outside Temps.
//! - `location_results`: what each location reported on the latest check,
//!   replaced on every check. Kept on the monitor, not on `status_checks`,
//!   which is a compressed hypertable: one row per monitor stays bounded.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE status_monitors \
             ADD COLUMN IF NOT EXISTS probe_from_nodes BOOLEAN NOT NULL DEFAULT FALSE, \
             ADD COLUMN IF NOT EXISTS location_results JSONB",
        )
        .await?;
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_constraint \
                              WHERE conname = 'status_monitors_probe_from_nodes_target') THEN \
                 ALTER TABLE status_monitors \
                   ADD CONSTRAINT status_monitors_probe_from_nodes_target \
                   CHECK (NOT probe_from_nodes OR check_kind <> 'environment'); \
               END IF; \
             END $$",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE status_monitors \
                 DROP CONSTRAINT IF EXISTS status_monitors_probe_from_nodes_target, \
                 DROP COLUMN IF EXISTS location_results, \
                 DROP COLUMN IF EXISTS probe_from_nodes",
            )
            .await?;
        Ok(())
    }
}
