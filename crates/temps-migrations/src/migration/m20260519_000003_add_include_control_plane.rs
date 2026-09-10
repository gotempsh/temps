// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration: add `include_control_plane` to `backup_schedules`.
//!
//! ## Purpose
//!
//! Previously every scheduled run fanned out to *both* the Temps control
//! plane (its own Postgres) AND the selected external services. That made
//! sense for "back up everything" schedules but was always a forced
//! tax-along on schedules that the operator scoped to a specific database
//! list — the run history would show a `control_plane` backup row next to
//! every Postgres/Redis backup whether they wanted it or not.
//!
//! With this column the operator picks per-schedule whether the control
//! plane is in scope, independently of `target_all_services`.
//!
//! ## Backfill
//!
//! Existing rows default to `TRUE` so existing runs keep producing the
//! control-plane backup. Operators opt out by editing the schedule.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "ALTER TABLE backup_schedules \
             ADD COLUMN IF NOT EXISTS include_control_plane BOOLEAN NOT NULL DEFAULT TRUE",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "ALTER TABLE backup_schedules DROP COLUMN IF EXISTS include_control_plane",
        )
        .await?;

        Ok(())
    }
}
