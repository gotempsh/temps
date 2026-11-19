// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conditions on an ignored error group (ADR 074, D1.3).
//!
//! An `ignored` group with none of these set keeps today's behaviour: the next
//! event reopens it. With conditions, ingest keeps it ignored until any one of
//! them is met, checked in the statement that already updates the group's
//! counter:
//!
//! - `ignore_until`: a point in time;
//! - `ignore_until_event_count`: the `total_count` at which it reopens;
//! - `ignore_until_user_count` / `ignore_user_count`: how many new distinct
//!   users reopen it, and how many have been seen since it was ignored;
//! - `ignore_forever`: it stays ignored whatever happens.
//!
//! Distinct users are counted exactly, and only while a user condition is
//! active, in `error_group_ignore_users`: one row per (group, user key). The
//! threshold is capped by the API (10,000), so the table holds at most that
//! many rows per ignored group, and its rows are deleted when the ignore ends.
//!
//! Every column is nullable or defaulted, so no backfill is needed and an
//! older binary ignores them.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE error_groups \
             ADD COLUMN IF NOT EXISTS ignore_until TIMESTAMPTZ, \
             ADD COLUMN IF NOT EXISTS ignore_until_event_count INTEGER, \
             ADD COLUMN IF NOT EXISTS ignore_until_user_count INTEGER, \
             ADD COLUMN IF NOT EXISTS ignore_user_count INTEGER NOT NULL DEFAULT 0, \
             ADD COLUMN IF NOT EXISTS ignore_forever BOOLEAN NOT NULL DEFAULT FALSE",
        )
        .await?;
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS error_group_ignore_users ( \
                 error_group_id INTEGER NOT NULL REFERENCES error_groups(id) ON DELETE CASCADE, \
                 user_key TEXT NOT NULL, \
                 PRIMARY KEY (error_group_id, user_key))",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS error_group_ignore_users")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE error_groups \
             DROP COLUMN IF EXISTS ignore_until, \
             DROP COLUMN IF EXISTS ignore_until_event_count, \
             DROP COLUMN IF EXISTS ignore_until_user_count, \
             DROP COLUMN IF EXISTS ignore_user_count, \
             DROP COLUMN IF EXISTS ignore_forever",
        )
        .await?;
        Ok(())
    }
}
