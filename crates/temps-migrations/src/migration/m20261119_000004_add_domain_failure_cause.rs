// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record why a domain's last certificate attempt failed.
//!
//! `domains.failure_cause` holds a typed cause (`unreachable`, `dns_lookup`,
//! `upstream_answered`, `rate_limited`, ...) classified from the CA's problem
//! type, so the console can explain the failure and the retry logic can tell a
//! CA hiccup from a closed port that will not fix itself. `last_error` keeps
//! the full text.
//!
//! Nullable with no default: existing rows read as "no recorded cause" until
//! their next attempt, and the previous binary ignores the column, so a
//! rollback keeps working. No backfill.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE domains ADD COLUMN IF NOT EXISTS failure_cause text")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE domains DROP COLUMN IF EXISTS failure_cause")
            .await?;
        Ok(())
    }
}
