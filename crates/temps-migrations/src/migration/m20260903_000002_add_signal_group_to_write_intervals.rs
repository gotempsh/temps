// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-043 §3 (interval ledger): add `signal_group` discriminant to
//! `project_telemetry_write_intervals`.
//!
//! The write-interval ledger (created in `m20260901_000006_create_telemetry_write_ledger`)
//! tracks where a project's telemetry went for the purpose of read routing
//! (ADR-041 §8). Until now it served the single `cloud_telemetry_write_mode`
//! (spans). ADR-043 §3 adds a second switch (`cloud_analytics_write_mode`) that
//! covers analytics events, metrics and proxy logs as a group.
//!
//! Rather than a second table, the same ledger gains a `signal_group` column.
//! The routing decorator for each domain filters on its own signal group and
//! gets an independent interval history. This is the same generalisation
//! `cloud_span_outbox → cloud_telemetry_outbox` applies to the outbox.
//!
//! # Changes
//!
//! 1. Adds `signal_group TEXT NOT NULL DEFAULT 'spans'` with a CHECK constraint.
//!    Existing rows (all span-related) correctly default to `'spans'` — no data
//!    migration needed.
//!
//! 2. Drops the existing partial-unique index
//!    `idx_write_intervals_one_open_per_project (project_id) WHERE effective_to IS NULL`
//!    and replaces it with one keyed on `(project_id, signal_group)`. This
//!    preserves the invariant that "at most one open interval per project per
//!    signal group exists", which is what the read router relies on to answer
//!    "where does this project's analytics go right now" without ambiguity.
//!
//! 3. Drops `idx_write_intervals_project_range (project_id, effective_from DESC)`
//!    and replaces it with `(project_id, signal_group, effective_from DESC)`.
//!    All queries on this table filter by both project_id and signal_group
//!    together; the old index would become a covering-but-wider scan after the
//!    column is added.

use sea_orm_migration::prelude::*;

const SIGNAL_GROUP_CONSTRAINT: &str = "project_telemetry_write_intervals_signal_group_valid";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Step 1: add the signal_group discriminant column.
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_telemetry_write_intervals \
                 ADD COLUMN signal_group TEXT NOT NULL DEFAULT 'spans'",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE project_telemetry_write_intervals \
                 ADD CONSTRAINT {SIGNAL_GROUP_CONSTRAINT} \
                 CHECK (signal_group IN ('spans', 'analytics'))"
            ))
            .await?;

        // Step 2: replace the per-project open-interval uniqueness constraint.
        // The old one allowed one open interval per project. The new one allows
        // one per project per signal group — so span routing and analytics
        // routing have independent interval histories that can be opened and
        // closed independently.
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_write_intervals_one_open_per_project")
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS \
                 idx_write_intervals_one_open_per_project_signal \
                 ON project_telemetry_write_intervals (project_id, signal_group) \
                 WHERE effective_to IS NULL",
            )
            .await?;

        // Step 3: replace the range-resolution index.
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_write_intervals_project_range")
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_write_intervals_project_signal_range \
                 ON project_telemetry_write_intervals \
                 (project_id, signal_group, effective_from DESC)",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reverse Step 3.
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_write_intervals_project_signal_range")
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_write_intervals_project_range \
                 ON project_telemetry_write_intervals (project_id, effective_from DESC)",
            )
            .await?;

        // Reverse Step 2.
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS \
                 idx_write_intervals_one_open_per_project_signal",
            )
            .await?;

        // Restore the original uniqueness constraint. By the time we reach this
        // point all rows have signal_group = 'spans' (the default), so at most
        // one open interval per project exists — the constraint is satisfiable.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS \
                 idx_write_intervals_one_open_per_project \
                 ON project_telemetry_write_intervals (project_id) \
                 WHERE effective_to IS NULL",
            )
            .await?;

        // Reverse Step 1.
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE project_telemetry_write_intervals \
                 DROP CONSTRAINT IF EXISTS {SIGNAL_GROUP_CONSTRAINT}"
            ))
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE project_telemetry_write_intervals \
                 DROP COLUMN IF EXISTS signal_group",
            )
            .await?;

        Ok(())
    }
}
