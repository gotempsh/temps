// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One bulk Cloud-telemetry activation job (ADR-042 §8).
//!
//! A job is the whole of "switch these projects to Cloud-primary and ship their
//! history", as one durable, resumable, cancellable unit. Its per-project rows
//! live in [`super::cloud_telemetry_bulk_job_projects`].
//!
//! At most one job may be `pending` or `running` at a time — enforced by a
//! partial unique index, because submission concurrency is 1 globally
//! (ADR-041 §3b) and a second job would contend for a submission scope that is
//! exclusive process-wide.

use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
use utoipa::ToSchema;

/// Which entry point created the job (ADR-042 §1).
///
/// The engine does not branch on this. It exists so an operator disputing an
/// invoice, or reading an audit trail, can tell "this instance spent money
/// because someone clicked a button" apart from "…because a purchase completed
/// and the payment was the authorization".
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
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum BulkJobTrigger {
    /// Created automatically by a successful Cloud enrollment. Has no
    /// `requested_by_user_id`: the payment is the authorization.
    #[sea_orm(string_value = "purchase")]
    Purchase,
    /// Created by an explicit console/CLI action, after a confirmed estimate.
    #[sea_orm(string_value = "operator")]
    Operator,
}

impl std::fmt::Display for BulkJobTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BulkJobTrigger::Purchase => write!(f, "purchase"),
            BulkJobTrigger::Operator => write!(f, "operator"),
        }
    }
}

/// Lifecycle of a bulk job.
///
/// The three terminal-with-a-problem states are deliberately distinct.
/// `completed_with_failures` means the job ran to the end and some projects
/// did not; `aborted` means an instance-wide condition stopped it and the
/// untouched projects are still `pending`, ready to resume; `cancelled` means
/// an operator asked it to stop. Collapsing any two of those would leave the
/// Console unable to say what to do next.
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
pub enum BulkJobStatus {
    /// Created, not yet picked up by the worker.
    #[default]
    #[sea_orm(string_value = "pending")]
    Pending,
    /// The worker is processing its projects, one at a time.
    #[sea_orm(string_value = "running")]
    Running,
    /// Every project reached `done` or `skipped`.
    #[sea_orm(string_value = "completed")]
    Completed,
    /// Every project reached a terminal state and at least one is `failed`.
    #[sea_orm(string_value = "completed_with_failures")]
    CompletedWithFailures,
    /// An instance-wide condition (`NotLinked`, `CredentialRejected`,
    /// `TelemetryExportDisabled`) stopped the job. Remaining projects are still
    /// `pending`, so a resume costs nothing already paid for.
    #[sea_orm(string_value = "aborted")]
    Aborted,
    /// An operator asked it to stop, and it stopped at a chunk boundary.
    #[sea_orm(string_value = "cancelled")]
    Cancelled,
}

impl BulkJobStatus {
    /// Whether the worker should pick this job up.
    pub fn is_active(&self) -> bool {
        matches!(self, BulkJobStatus::Pending | BulkJobStatus::Running)
    }

    /// Whether the job has stopped for good.
    pub fn is_terminal(&self) -> bool {
        !self.is_active()
    }
}

impl std::fmt::Display for BulkJobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BulkJobStatus::Pending => write!(f, "pending"),
            BulkJobStatus::Running => write!(f, "running"),
            BulkJobStatus::Completed => write!(f, "completed"),
            BulkJobStatus::CompletedWithFailures => write!(f, "completed_with_failures"),
            BulkJobStatus::Aborted => write!(f, "aborted"),
            BulkJobStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cloud_telemetry_bulk_jobs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(column_name = "trigger")]
    pub trigger: BulkJobTrigger,
    /// `None` on the purchase path — there is no operator to attribute it to.
    pub requested_by_user_id: Option<i32>,
    pub status: BulkJobStatus,
    /// Sum of the per-project pre-send estimates.
    pub estimated_spans: i64,
    pub estimated_bytes: i64,
    /// Spans Temps Cloud has acknowledged across every project so far.
    /// Advanced per completed chunk, so a restart resumes the running total
    /// rather than restarting it at zero.
    pub spans_shipped: i64,
    pub bytes_shipped: i64,
    /// Binds an operator-path job to the exact estimate that was confirmed.
    pub plan_hash: Option<String>,
    pub created_at: DBDateTime,
    pub started_at: Option<DBDateTime>,
    pub completed_at: Option<DBDateTime>,
    /// Set by a cancel request; honoured by the worker at the next chunk
    /// boundary.
    pub cancel_requested_at: Option<DBDateTime>,
    /// The one actionable reason an instance-wide failure stopped the job.
    pub abort_reason: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::cloud_telemetry_bulk_job_projects::Entity")]
    Projects,
}

impl Related<super::cloud_telemetry_bulk_job_projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_job_defaults_to_pending_so_the_worker_picks_it_up() {
        assert_eq!(BulkJobStatus::default(), BulkJobStatus::Pending);
        assert!(BulkJobStatus::default().is_active());
    }

    #[test]
    fn exactly_the_two_pre_terminal_states_are_active() {
        // The worker's resume-on-boot query is "find an active job". Getting
        // this set wrong either strands a job forever or restarts a finished
        // one — and restarting a finished one re-ships history the customer
        // already paid for.
        for status in [BulkJobStatus::Pending, BulkJobStatus::Running] {
            assert!(status.is_active(), "{status} must be active");
            assert!(!status.is_terminal(), "{status} must not be terminal");
        }
        for status in [
            BulkJobStatus::Completed,
            BulkJobStatus::CompletedWithFailures,
            BulkJobStatus::Aborted,
            BulkJobStatus::Cancelled,
        ] {
            assert!(!status.is_active(), "{status} must not be active");
            assert!(status.is_terminal(), "{status} must be terminal");
        }
    }

    #[test]
    fn status_display_matches_the_persisted_column_values() {
        // These strings are also the migration's CHECK constraint. A mismatch
        // would be an insert that fails only in production.
        assert_eq!(BulkJobStatus::Pending.to_string(), "pending");
        assert_eq!(BulkJobStatus::Running.to_string(), "running");
        assert_eq!(BulkJobStatus::Completed.to_string(), "completed");
        assert_eq!(
            BulkJobStatus::CompletedWithFailures.to_string(),
            "completed_with_failures"
        );
        assert_eq!(BulkJobStatus::Aborted.to_string(), "aborted");
        assert_eq!(BulkJobStatus::Cancelled.to_string(), "cancelled");
        assert_eq!(BulkJobTrigger::Purchase.to_string(), "purchase");
        assert_eq!(BulkJobTrigger::Operator.to_string(), "operator");
    }

    #[test]
    fn status_and_trigger_serde_use_the_wire_names_the_console_switches_on() {
        for (status, wire) in [
            (BulkJobStatus::Pending, r#""pending""#),
            (BulkJobStatus::Running, r#""running""#),
            (BulkJobStatus::Completed, r#""completed""#),
            (
                BulkJobStatus::CompletedWithFailures,
                r#""completed_with_failures""#,
            ),
            (BulkJobStatus::Aborted, r#""aborted""#),
            (BulkJobStatus::Cancelled, r#""cancelled""#),
        ] {
            assert_eq!(
                serde_json::to_string(&status).expect("status must serialize"),
                wire
            );
            assert_eq!(
                serde_json::from_str::<BulkJobStatus>(wire).expect("status must deserialize"),
                status
            );
        }
        for (trigger, wire) in [
            (BulkJobTrigger::Purchase, r#""purchase""#),
            (BulkJobTrigger::Operator, r#""operator""#),
        ] {
            assert_eq!(
                serde_json::to_string(&trigger).expect("trigger must serialize"),
                wire
            );
            assert_eq!(
                serde_json::from_str::<BulkJobTrigger>(wire).expect("trigger must deserialize"),
                trigger
            );
        }
    }
}
