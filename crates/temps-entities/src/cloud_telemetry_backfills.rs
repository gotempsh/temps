// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Progress of the out-of-process Temps Cloud telemetry backfill (ADR-040 §1).
//!
//! `temps backfill cloud-telemetry` runs *outside* `temps serve` on purpose, so
//! its own `indicatif` progress bar and local resume checkpoint are visible only
//! in the terminal that launched it. That is fine for the operator who typed the
//! command and useless for everyone else: someone looking at the Console has no
//! way to tell whether a backfill is running, stalled, finished, or was never
//! started at all.
//!
//! This table is the shared, queryable half of that state. The CLI upserts one
//! row per project at the same cadence it already persists its local checkpoint
//! (once per chunk), so it costs one cheap metadata `UPDATE` per chunk and adds
//! no contention to the telemetry tables the backfill reads.
//!
//! **Absence of a row means [`CloudTelemetryBackfillStatus::NotStarted`].** The
//! read API materialises that rather than 404-ing, because "never run" and
//! "broken" need completely different UI.

use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

/// Lifecycle of the most recent backfill run for a project.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum CloudTelemetryBackfillStatus {
    /// No backfill has ever been recorded for this project. Never persisted —
    /// it is what the read API reports when no row exists.
    #[default]
    #[sea_orm(string_value = "not_started")]
    NotStarted,
    /// A run is in progress. `updated_at` is the liveness signal: a `running`
    /// row that has not moved is a stalled or killed run, and the Console can
    /// say so instead of spinning forever.
    #[sea_orm(string_value = "running")]
    Running,
    /// The run walked its whole window and Cloud acknowledged everything.
    #[sea_orm(string_value = "completed")]
    Completed,
    /// The run stopped early. `last_error` carries why, verbatim, because the
    /// operator who reads it in the Console is not the one who saw the
    /// terminal output.
    #[sea_orm(string_value = "failed")]
    Failed,
}

impl std::fmt::Display for CloudTelemetryBackfillStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudTelemetryBackfillStatus::NotStarted => write!(f, "not_started"),
            CloudTelemetryBackfillStatus::Running => write!(f, "running"),
            CloudTelemetryBackfillStatus::Completed => write!(f, "completed"),
            CloudTelemetryBackfillStatus::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cloud_telemetry_backfills")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// One row per project: a new run replaces the previous run's progress
    /// rather than accumulating history. The Console asks "what is happening
    /// now", not "what happened in March".
    #[sea_orm(unique)]
    pub project_id: i32,
    pub status: CloudTelemetryBackfillStatus,
    /// Spans Temps Cloud has acknowledged so far in this run.
    pub spans_processed: i64,
    /// Total spans the run expects to send, taken from the same estimate
    /// `--dry-run` prints. Zero when the window turned out to be empty.
    pub spans_total: i64,
    /// The `[from, to]` window the operator asked for, so the Console can say
    /// *which* history is being filled rather than just "a backfill".
    pub window_from: Option<DBDateTime>,
    pub window_to: Option<DBDateTime>,
    pub started_at: Option<DBDateTime>,
    /// Bumped on every progress write. The Console uses it to distinguish a
    /// live run from an abandoned one.
    pub updated_at: DBDateTime,
    pub completed_at: Option<DBDateTime>,
    /// Why the last run stopped, when `status` is `failed`. Verbatim, because
    /// a self-hosted operator has nobody to ask what a generic failure meant.
    pub last_error: Option<String>,
    /// The bulk activation job driving this run, when one is (ADR-042 §6/§8).
    ///
    /// `None` for a run started by `temps backfill cloud-telemetry`, which is
    /// still the offline/recovery tool and is unchanged. The per-project
    /// backfill card reads this so a project's progress is never mysteriously
    /// "already running" with no explanation of who started it.
    pub bulk_job_id: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_status_is_not_started() {
        // The read API reports this when no row exists; it must never be
        // confused with a completed run.
        assert_eq!(
            CloudTelemetryBackfillStatus::default(),
            CloudTelemetryBackfillStatus::NotStarted
        );
    }

    #[test]
    fn status_display_matches_the_persisted_column_values() {
        assert_eq!(
            CloudTelemetryBackfillStatus::NotStarted.to_string(),
            "not_started"
        );
        assert_eq!(CloudTelemetryBackfillStatus::Running.to_string(), "running");
        assert_eq!(
            CloudTelemetryBackfillStatus::Completed.to_string(),
            "completed"
        );
        assert_eq!(CloudTelemetryBackfillStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn status_serde_uses_the_wire_names_the_console_switches_on() {
        for (status, wire) in [
            (CloudTelemetryBackfillStatus::NotStarted, r#""not_started""#),
            (CloudTelemetryBackfillStatus::Running, r#""running""#),
            (CloudTelemetryBackfillStatus::Completed, r#""completed""#),
            (CloudTelemetryBackfillStatus::Failed, r#""failed""#),
        ] {
            assert_eq!(
                serde_json::to_string(&status).expect("status must serialize"),
                wire
            );
            assert_eq!(
                serde_json::from_str::<CloudTelemetryBackfillStatus>(wire)
                    .expect("status must deserialize"),
                status
            );
        }
    }
}
