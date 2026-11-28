// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How each external plugin is isolated: in a hardened container (the default
//! for every plugin installed from now on) or as a host process.
//!
//! A plugin that is already installed keeps running the way it always has, as
//! a process, so an upgrade changes nothing about a working instance; an
//! administrator moves it into a container from the console when ready. That
//! is why the column is added with the default `process`, which fills every
//! existing row without rewriting the table, and only then switched to
//! `container` for rows inserted afterwards.
//!
//! The setting lives on the actor row because it is a decision about one
//! installed plugin: it follows the plugin through updates from the same
//! source and is made again when the plugin is reinstalled from another one.
//!
//! `down` drops the column. The previous binary never reads it, and runs
//! every plugin as a process.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             ADD COLUMN IF NOT EXISTS isolation TEXT NOT NULL DEFAULT 'process'",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors ALTER COLUMN isolation SET DEFAULT 'container'",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             DROP CONSTRAINT IF EXISTS external_plugin_actors_isolation_check",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             ADD CONSTRAINT external_plugin_actors_isolation_check \
             CHECK (isolation IN ('container', 'process'))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_plugin_actors \
             DROP CONSTRAINT IF EXISTS external_plugin_actors_isolation_check",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE external_plugin_actors DROP COLUMN IF EXISTS isolation")
            .await?;
        Ok(())
    }
}
