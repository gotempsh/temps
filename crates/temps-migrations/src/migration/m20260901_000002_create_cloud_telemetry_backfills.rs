// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared progress record for the Temps Cloud telemetry backfill (ADR-040 §1).
//!
//! `temps backfill cloud-telemetry` runs out of process, so its terminal
//! progress bar and its local resume checkpoint are invisible to `temps serve`
//! and to the Console. An operator looking at the UI would have no way to tell
//! a run that is progressing from one that died an hour ago — which is exactly
//! the silent-failure mode CLAUDE.md's discoverability rule exists to prevent.
//!
//! One row per project, upserted by the CLI once per chunk (the same cadence it
//! already writes its local checkpoint), so this adds one small metadata
//! `UPDATE` per chunk and no contention on the span tables being read.
//!
//! Absence of a row means "never started" and is materialised as such by the
//! read API, so the Console can tell "not set up yet" apart from "broken".

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE cloud_telemetry_backfills (
    id SERIAL PRIMARY KEY,
    -- One row per project. A new run replaces the previous run's progress
    -- rather than accumulating history: the Console asks "what is happening
    -- now", and an unbounded history table would need its own retention.
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'running',
    spans_processed BIGINT NOT NULL DEFAULT 0,
    spans_total BIGINT NOT NULL DEFAULT 0,
    window_from TIMESTAMPTZ,
    window_to TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    -- Bumped on every progress write; the Console reads it as a liveness
    -- signal so a killed run shows as stalled instead of spinning forever.
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    -- Verbatim failure reason. The operator reading it in the Console is not
    -- the one who saw the terminal output.
    last_error TEXT,
    CONSTRAINT cloud_telemetry_backfills_project_unique UNIQUE (project_id),
    -- 'not_started' is deliberately absent: it is the *absence* of a row, and
    -- allowing it to be stored too would create two spellings of one state.
    CONSTRAINT cloud_telemetry_backfills_status_valid
        CHECK (status IN ('running', 'completed', 'failed'))
);
"#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS cloud_telemetry_backfills")
            .await?;

        Ok(())
    }
}
