// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record, on a restore run, the login its restored data answers to.
//!
//! A cross-service restore leaves the target's data answering to the
//! origin's login, and the target's stored configuration is patched to it
//! once the restore succeeds. A restore on a worker node can outlive the
//! process that started it, and the process that settles it after a restart
//! must patch the login captured when the restore started, not whatever the
//! origin's login has become since.
//!
//! `restored_login_encrypted` holds that login, encrypted with
//! `EncryptionService` (it carries passwords). `NULL` on runs that predate
//! this column, and on every run that is not a worker restore.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE restore_runs ADD COLUMN IF NOT EXISTS restored_login_encrypted TEXT",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE restore_runs DROP COLUMN IF EXISTS restored_login_encrypted",
            )
            .await?;
        Ok(())
    }
}
