// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One project's slot in a bulk Cloud-telemetry activation job (ADR-042 §8).
//!
//! The row carries three things the job cannot recompute after a restart:
//!
//! 1. **The window.** Recomputing "everything local storage holds" after a
//!    restart would produce a different answer than at enqueue time, and the
//!    difference is silently unshipped history.
//! 2. **The resume cursor.** `resume_start_time` / `resume_row_id` /
//!    `resume_span_id` are exactly `CloudBackfillCursor`'s three
//!    fields. A scoped Cloud submission persists nothing resumable of its own
//!    (ADR-042 P0), so this row is the only place a restart can learn where the
//!    project got to. Without it a restart either re-ships history the customer
//!    already paid for, or skips it.
//! 3. **What already shipped.** So the job's totals survive a restart instead
//!    of restarting the count at zero.
//!
//! `project_id` carries no foreign key on purpose, matching `cloud_span_outbox`
//! and the write-mode ledger: deleting a project must not be blocked by
//! telemetry bookkeeping.

use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

/// Where one project has got to inside a bulk job.
///
/// `switching` and `backfilling` are separate on purpose: the switch is cheap,
/// atomic and egresses nothing, while the backfill can run for hours and costs
/// money. An operator watching a stuck job needs to know which of the two it is
/// stuck in, and a project that failed after switching is *not* rolled back
/// (ADR-042 §7) — so the two must be distinguishable after the fact too.
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
pub enum BulkJobProjectStatus {
    /// Not started, or deliberately left untouched by an aborted job so a
    /// resume can pick it up.
    #[default]
    #[sea_orm(string_value = "pending")]
    Pending,
    /// `set_write_mode(project, Cloud)` is running.
    #[sea_orm(string_value = "switching")]
    Switching,
    /// The switch landed; history is being shipped.
    #[sea_orm(string_value = "backfilling")]
    Backfilling,
    /// Switched and fully backfilled.
    #[sea_orm(string_value = "done")]
    Done,
    /// Switched, but the backfill stopped. `last_error` says why. **The switch
    /// is never rolled back** — a failed backfill leaves a known, recorded,
    /// retryable hole in history rather than a silently bisected timeline.
    #[sea_orm(string_value = "failed")]
    Failed,
    /// Never switched, because a prerequisite the job must not change on the
    /// operator's behalf was missing. `skip_reason` says which.
    #[sea_orm(string_value = "skipped")]
    Skipped,
}

impl BulkJobProjectStatus {
    /// Whether this project still needs the worker.
    pub fn is_pending(&self) -> bool {
        matches!(
            self,
            BulkJobProjectStatus::Pending
                | BulkJobProjectStatus::Switching
                | BulkJobProjectStatus::Backfilling
        )
    }

    /// Whether this project is finished, whatever the outcome.
    pub fn is_terminal(&self) -> bool {
        !self.is_pending()
    }
}

impl std::fmt::Display for BulkJobProjectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BulkJobProjectStatus::Pending => write!(f, "pending"),
            BulkJobProjectStatus::Switching => write!(f, "switching"),
            BulkJobProjectStatus::Backfilling => write!(f, "backfilling"),
            BulkJobProjectStatus::Done => write!(f, "done"),
            BulkJobProjectStatus::Failed => write!(f, "failed"),
            BulkJobProjectStatus::Skipped => write!(f, "skipped"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cloud_telemetry_bulk_job_projects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub job_id: Uuid,
    pub project_id: i32,
    pub status: BulkJobProjectStatus,
    /// Machine-readable skip reason, e.g. `fidelity_not_queryable`. Paired in
    /// the UI with a direct link to the setting that unblocks it.
    pub skip_reason: Option<String>,
    pub window_from: DBDateTime,
    pub window_to: DBDateTime,
    pub estimated_spans: i64,
    pub estimated_bytes: i64,
    pub spans_shipped: i64,
    pub bytes_shipped: i64,
    /// `CloudBackfillCursor::last_start_time`, persisted per completed chunk.
    pub resume_start_time: Option<DBDateTime>,
    /// `CloudBackfillCursor::last_row_id` — the Postgres tiebreaker.
    pub resume_row_id: Option<i64>,
    /// `CloudBackfillCursor::last_span_id` — the ClickHouse tiebreaker.
    pub resume_span_id: Option<String>,
    /// Truncated failure reason, when `status` is `failed`.
    pub last_error: Option<String>,
    pub started_at: Option<DBDateTime>,
    pub completed_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cloud_telemetry_bulk_jobs::Entity",
        from = "Column::JobId",
        to = "super::cloud_telemetry_bulk_jobs::Column::Id",
        on_delete = "Cascade"
    )]
    Job,
}

impl Related<super::cloud_telemetry_bulk_jobs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Job.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_project_row_defaults_to_pending() {
        assert_eq!(
            BulkJobProjectStatus::default(),
            BulkJobProjectStatus::Pending
        );
    }

    #[test]
    fn the_three_in_flight_states_are_pending_and_the_three_outcomes_are_terminal() {
        // Job completion is "every project is terminal". Counting `switching`
        // or `backfilling` as terminal would declare a job complete while it
        // was still spending money.
        for status in [
            BulkJobProjectStatus::Pending,
            BulkJobProjectStatus::Switching,
            BulkJobProjectStatus::Backfilling,
        ] {
            assert!(status.is_pending(), "{status} must still need the worker");
            assert!(!status.is_terminal(), "{status} must not be terminal");
        }
        for status in [
            BulkJobProjectStatus::Done,
            BulkJobProjectStatus::Failed,
            BulkJobProjectStatus::Skipped,
        ] {
            assert!(status.is_terminal(), "{status} must be terminal");
            assert!(!status.is_pending(), "{status} must not need the worker");
        }
    }

    #[test]
    fn status_display_matches_the_persisted_column_values() {
        for (status, wire) in [
            (BulkJobProjectStatus::Pending, "pending"),
            (BulkJobProjectStatus::Switching, "switching"),
            (BulkJobProjectStatus::Backfilling, "backfilling"),
            (BulkJobProjectStatus::Done, "done"),
            (BulkJobProjectStatus::Failed, "failed"),
            (BulkJobProjectStatus::Skipped, "skipped"),
        ] {
            assert_eq!(status.to_string(), wire);
            assert_eq!(
                serde_json::to_string(&status).expect("status must serialize"),
                format!("\"{wire}\"")
            );
        }
    }
}
