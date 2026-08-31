// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Make `alarms.project_id` nullable.
//!
//! Some alarm sources are genuinely host/control-plane-wide and have no
//! associated project — disk space on the host, and worker node offline /
//! resource-pressure alerts. The original schema declared `project_id`
//! `NOT NULL`, which forced those alarms to either misattribute themselves to
//! an arbitrary project or skip persistence entirely (the latter is what
//! actually happened — they only ever sent a notification directly).
//!
//! Fix: drop NOT NULL and the original `ON DELETE CASCADE` FK, recreate it as
//! `ON DELETE SET NULL` so a deleted project doesn't take a system-wide
//! alarm's history with it. `NULL` project_id alarms are surfaced via a
//! separate "system alarms" endpoint, not the per-project one.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared("ALTER TABLE alarms DROP CONSTRAINT IF EXISTS fk_alarms_project")
            .await?;

        db.execute_unprepared("ALTER TABLE alarms ALTER COLUMN project_id DROP NOT NULL")
            .await?;

        db.execute_unprepared(
            "ALTER TABLE alarms ADD CONSTRAINT fk_alarms_project \
             FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE SET NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // Rows with NULL project_id can only exist as a result of the new
        // behaviour (system-wide alarms) and have no meaningful FK target,
        // so they're deleted before NOT NULL is restored.
        db.execute_unprepared("DELETE FROM alarms WHERE project_id IS NULL")
            .await?;

        db.execute_unprepared("ALTER TABLE alarms DROP CONSTRAINT IF EXISTS fk_alarms_project")
            .await?;

        db.execute_unprepared("ALTER TABLE alarms ALTER COLUMN project_id SET NOT NULL")
            .await?;

        db.execute_unprepared(
            "ALTER TABLE alarms ADD CONSTRAINT fk_alarms_project \
             FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE",
        )
        .await?;

        Ok(())
    }
}
