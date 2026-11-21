// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scheduled maintenance windows on status pages (ADR 108 D2).
//!
//! A window has a public title and description, a start and an end, and the
//! page's monitors it covers. While it is in progress the covered monitors'
//! checks are left out of the uptime percentage and raise no outage alert
//! or incident. Cancelling sets `cancelled_at`; a cancelled window counts
//! for nothing.
//!
//! - `status_page_maintenances`: cascades with the page.
//! - `status_page_maintenance_monitors`: cascades with the window and with
//!   the monitor. Indexed by monitor for the per-check "is it in
//!   maintenance" lookup.
//!
//! Both tables are new and small, so plain indexes are fine.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS status_page_maintenances (
                id SERIAL PRIMARY KEY,
                status_page_id INTEGER NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
                title TEXT NOT NULL,
                description TEXT,
                starts_at TIMESTAMPTZ NOT NULL,
                ends_at TIMESTAMPTZ NOT NULL,
                cancelled_at TIMESTAMPTZ,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                CONSTRAINT status_page_maintenances_ends_after_start CHECK (ends_at > starts_at)
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_status_page_maintenances_page_end \
             ON status_page_maintenances(status_page_id, ends_at)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS status_page_maintenance_monitors (
                maintenance_id INTEGER NOT NULL
                    REFERENCES status_page_maintenances(id) ON DELETE CASCADE,
                monitor_id INTEGER NOT NULL REFERENCES status_monitors(id) ON DELETE CASCADE,
                PRIMARY KEY (maintenance_id, monitor_id)
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_status_page_maintenance_monitors_monitor \
             ON status_page_maintenance_monitors(monitor_id)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS status_page_maintenance_monitors")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS status_page_maintenances")
            .await?;
        Ok(())
    }
}
