// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An optional password on a status page (ADR 108 D2).
//!
//! - `password_hash`: argon2id PHC string; `NULL` means the page is open.
//! - `password_version`: bumped whenever the password is set, changed or
//!   removed. It is signed into every access cookie, so any change ends
//!   every session at once.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE status_pages \
                 ADD COLUMN IF NOT EXISTS password_hash TEXT, \
                 ADD COLUMN IF NOT EXISTS password_version INTEGER NOT NULL DEFAULT 0",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE status_pages \
                 DROP COLUMN IF EXISTS password_version, \
                 DROP COLUMN IF EXISTS password_hash",
            )
            .await?;
        Ok(())
    }
}
