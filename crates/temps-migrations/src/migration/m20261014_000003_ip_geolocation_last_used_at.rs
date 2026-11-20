// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record when an `ip_geolocations` row was last handed out to a writer.
//!
//! `updated_at` only advances once per `geo.stale_lookup_days`, so it cannot
//! tell the retention cleanup that an address is still being looked up and
//! cached by the proxy. `last_used_at` can: the lookup service advances it at
//! most once every twelve hours, and the cleanup only deletes addresses it has not
//! advanced within the retention window.
//!
//! The column is added with `DEFAULT now()`. A constant-per-statement default
//! is applied by PostgreSQL 11+ as a catalog-only change: no table rewrite and
//! no per-row work, so the migration holds its lock for an instant and never
//! queues address lookups behind a long `UPDATE`. Every row that already
//! exists reads as last used at the migration time, which is what is wanted:
//! `updated_at` only advances every `geo.stale_lookup_days`, so it says nothing
//! about when an address was last handed to a proxy log that may still be
//! retained outside PostgreSQL, and treating it as the last use would let the
//! first sweep delete addresses that logs written days ago still point at.
//! Stamping gives every existing address one full retention window to be seen
//! again. A column that already exists without the default (a development
//! database) keeps its NULLs, which the cleanup reads through
//! `COALESCE(last_used_at, updated_at)`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DO $$ BEGIN \
                   IF EXISTS (SELECT 1 FROM information_schema.tables \
                              WHERE table_schema = current_schema() \
                                AND table_name = 'ip_geolocations') THEN \
                     ALTER TABLE ip_geolocations \
                       ADD COLUMN IF NOT EXISTS last_used_at TIMESTAMPTZ DEFAULT now(); \
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
                "ALTER TABLE IF EXISTS ip_geolocations DROP COLUMN IF EXISTS last_used_at",
            )
            .await?;
        Ok(())
    }
}
