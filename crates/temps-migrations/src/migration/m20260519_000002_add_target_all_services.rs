// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Migration: add `target_all_services` to `backup_schedules`.
//!
//! ## Purpose
//!
//! Restores the "back up every database" default in a controllable way.
//! After `m20260519_000001_create_backup_schedule_services`, schedules with
//! no attached rows produced only the control-plane backup — that's the
//! right behaviour when an operator explicitly scopes down, but a bad
//! default for "I just want all my DBs backed up forever, including future
//! ones."
//!
//! With this column:
//!   - `target_all_services = true`  → fan-out loads every external service
//!     (auto-includes future databases).
//!   - `target_all_services = false` → fan-out uses the explicit
//!     `backup_schedule_services` membership table.
//!
//! ## Backfill
//!
//! Existing schedules backfill to `TRUE`. This is the safer default for
//! operators upgrading from the previous migration, which had effectively
//! disabled service backups for legacy schedules.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "ALTER TABLE backup_schedules \
             ADD COLUMN IF NOT EXISTS target_all_services BOOLEAN NOT NULL DEFAULT TRUE",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "ALTER TABLE backup_schedules DROP COLUMN IF EXISTS target_all_services",
        )
        .await?;

        Ok(())
    }
}
