// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Merging and splitting error groups (ADR 074, D1.2).
//!
//! Ingest already resolves an event's fingerprint to a group through
//! `error_group_fingerprints`, so a merge is a repointing of those rows: every
//! fingerprint of the merged-away group points at the surviving group, and
//! future events land there with no change to the ingest path.
//!
//! Two nullable columns carry it:
//!
//! - `error_groups.merged_into_group_id` marks a group that was merged away.
//!   It keeps its own events and its own `total_count` (the surviving group
//!   adds that count to its own), so unmerging restores both exactly. Lists
//!   and status counts skip these rows; the group is still readable by id, so
//!   a link to it explains where it went.
//! - `error_group_fingerprints.merged_from_group_id` records which group a
//!   fingerprint was moved away from, which is what unmerge moves it back to.
//!   `NULL` means the fingerprint belongs to the group it points at, which is
//!   the state every existing row is in.
//!
//! Both default to NULL, so existing rows need no backfill and an older binary
//! keeps working against the new schema: it ignores the columns, and a merge
//! performed by a newer binary still routes events correctly, because the
//! fingerprint rows it reads already point at the surviving group.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE error_groups ADD COLUMN IF NOT EXISTS merged_into_group_id INTEGER \
             REFERENCES error_groups(id) ON DELETE SET NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE error_group_fingerprints ADD COLUMN IF NOT EXISTS \
             merged_from_group_id INTEGER REFERENCES error_groups(id) ON DELETE SET NULL",
        )
        .await?;
        // Partial indexes: only merged rows are ever looked up this way, and
        // on an install that never merges anything both stay empty. The
        // tables are small (one row per group / per fingerprint), so these
        // build without the concurrent path.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_groups_merged_into \
             ON error_groups(merged_into_group_id) WHERE merged_into_group_id IS NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_error_group_fingerprints_merged_from \
             ON error_group_fingerprints(merged_from_group_id) \
             WHERE merged_from_group_id IS NOT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Unmerge every merge first, so dropping the columns cannot leave a
        // fingerprint pointing at a group the user never merged it into.
        db.execute_unprepared(
            "UPDATE error_group_fingerprints SET error_group_id = merged_from_group_id, \
             merged_from_group_id = NULL WHERE merged_from_group_id IS NOT NULL",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE error_groups target SET total_count = GREATEST(target.total_count - \
             moved.count, 0) FROM (SELECT merged_into_group_id AS id, \
             COALESCE(SUM(total_count), 0)::bigint AS count FROM error_groups \
             WHERE merged_into_group_id IS NOT NULL GROUP BY merged_into_group_id) moved \
             WHERE target.id = moved.id",
        )
        .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_error_group_fingerprints_merged_from")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS idx_error_groups_merged_into")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE error_group_fingerprints DROP COLUMN IF EXISTS merged_from_group_id",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE error_groups DROP COLUMN IF EXISTS merged_into_group_id")
            .await?;
        Ok(())
    }
}
