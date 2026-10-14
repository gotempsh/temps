// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record when an `ip_geolocations` row was last handed out to a writer.
//!
//! `updated_at` only advances once per `geo.stale_lookup_days`, so it cannot
//! tell the retention cleanup that an address is still being looked up and
//! cached by the proxy. `last_used_at` can: the lookup service advances it at
//! most once an hour, and the cleanup only deletes addresses it has not
//! advanced within the retention window.
//!
//! The column is nullable with no default, which is a catalog-only change on
//! PostgreSQL: no table rewrite, no lock held beyond the `ALTER`.

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
                       ADD COLUMN IF NOT EXISTS last_used_at TIMESTAMPTZ; \
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
