// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pin a service's continuous, standing archiving process to one stable S3
//! source instead of silently re-pointing it on every backup run.
//!
//! Two mechanisms write to a destination that persists across backup runs
//! rather than being chosen fresh each time:
//!
//! - `postgres_walg` previously rewrote `archive_command` (and its
//!   `WALG_S3_PREFIX`) to whatever `s3_source_id` the *triggering* backup run
//!   happened to use — a manual run, or whichever of several enabled
//!   schedules last fired. WAL-G requires a base backup and the WAL segments
//!   covering its start/end LSN to live under the same S3 prefix to be
//!   restorable; once two schedules on the same service point at different
//!   S3 sources, continuous archiving flips between buckets on every run,
//!   and any base backup taken while pointed at the source that is no
//!   longer active has WAL segments that will never appear back where the
//!   currently-active source (or Cloud's mirror, which only ever tracks the
//!   Cloud-managed source) looks for them.
//! - MariaDB's binlog shipper (`ExternalServiceHealthMonitor::maybe_archive_mariadb_binlogs`)
//!   previously re-resolved its destination on every health-monitor tick by
//!   scanning enabled backup schedules ordered by `updated_at DESC` —
//!   meaning editing *any* covering schedule, even one unrelated to this
//!   service via `target_all_services`, could silently redirect an
//!   in-progress binlog stream mid-flight, splitting the chain a PITR
//!   restore needs across buckets.
//!
//! `continuous_archive_s3_source_id` pins the source once per service —
//! defaulting to Cloud's managed source when the instance is linked,
//! otherwise whatever source the service's first archiving run uses — and
//! both mechanisms now refuse to silently re-point it (see
//! `crates/temps-providers/src/continuous_archive.rs`, used by
//! `crates/temps-backup/src/engines/postgres_walg.rs`,
//! `crates/temps-backup/src/engines/mariadb_physical.rs`, and
//! `crates/temps-providers/src/health_monitor.rs`).
//! `continuous_archive_pinned_at` records when that pin was established (or
//! last deliberately changed via the explicit repoint operation) so Cloud's
//! Postgres mirror and MariaDB's PITR restore can tell "this data is still
//! catching up" (its own timestamp is after the pin) apart from "this data
//! was written to a since-abandoned source and can never appear here" (its
//! own timestamp is before the pin) — the latter is permanent, not worth
//! retrying forever.
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
             ADD COLUMN IF NOT EXISTS continuous_archive_s3_source_id INTEGER \
             REFERENCES s3_sources(id) ON DELETE SET NULL",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_services \
             ADD COLUMN IF NOT EXISTS continuous_archive_pinned_at TIMESTAMPTZ",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE external_services DROP COLUMN IF EXISTS continuous_archive_s3_source_id",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE external_services DROP COLUMN IF EXISTS continuous_archive_pinned_at",
        )
        .await?;
        Ok(())
    }
}
