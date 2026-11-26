// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record, on a restore run, which databases and login roles the restore
//! brought back.
//!
//! A project linked to a PostgreSQL service reads the database named for its
//! project and environment on this server. A backup from another server
//! carries that server's databases, so when the names differ the linked app
//! silently gets a new, empty database while the restored data sits unused.
//! The list read right after the restore lets the service page show what came
//! back and which linked project expects a database the backup does not have.
//!
//! `restored_inventory` is JSONB (`{"databases": [...], "roles": [...]}`),
//! names only. `NULL` on runs that predate this column, on engines other than
//! PostgreSQL, and when the list could not be read after the restore.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE restore_runs ADD COLUMN IF NOT EXISTS restored_inventory JSONB",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE restore_runs DROP COLUMN IF EXISTS restored_inventory")
            .await?;
        Ok(())
    }
}
