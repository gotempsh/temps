// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pin a Postgres service's continuous WAL-G archiving to one stable S3
//! source instead of silently re-pointing it on every backup run.
//!
//! `postgres_walg`'s engine previously rewrote `archive_command` (and its
//! `WALG_S3_PREFIX`) to whatever `s3_source_id` the *triggering* backup run
//! happened to use — a manual run, or whichever of several enabled schedules
//! last fired. WAL-G requires a base backup and the WAL segments covering its
//! start/end LSN to live under the same S3 prefix to be restorable; once two
//! schedules on the same service point at different S3 sources, continuous
//! archiving flips between buckets on every run, and any base backup taken
//! while pointed at the source that is no longer active has WAL segments
//! that will never appear back where the currently-active source (or Cloud's
//! mirror, which only ever tracks the Cloud-managed source) looks for them.
//!
//! `walg_archive_s3_source_id` pins the source once — defaulting to Cloud's
//! managed source when the instance is linked, otherwise whatever source the
//! service's first WAL-G run uses — and the engine now refuses to silently
//! re-point it (see `crates/temps-backup/src/engines/postgres_walg.rs`).
//! `walg_archive_pinned_at` records when that pin was established (or last
//! deliberately changed via the explicit repoint operation) so the Cloud
//! mirror can tell "this backup's WAL is still catching up" (`started_at` is
//! after the pin) apart from "this backup's WAL was written to a
//! since-abandoned source and can never appear here" (`started_at` is
//! before the pin) — the latter is permanent, not worth retrying forever.
//!
//! Both columns are nullable with no default: an existing service with only
//! ever one S3 source in play keeps behaving exactly as before until the
//! engine (or an operator, via the explicit repoint API) establishes a pin.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_services \
             ADD COLUMN IF NOT EXISTS walg_archive_s3_source_id INTEGER \
             REFERENCES s3_sources(id) ON DELETE SET NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_services \
             ADD COLUMN IF NOT EXISTS walg_archive_pinned_at TIMESTAMPTZ",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_services DROP COLUMN IF EXISTS walg_archive_s3_source_id",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_services DROP COLUMN IF EXISTS walg_archive_pinned_at",
        )
        .await?;
        Ok(())
    }
}
