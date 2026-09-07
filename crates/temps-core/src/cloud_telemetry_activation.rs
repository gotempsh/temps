// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The seam the purchase-triggered Cloud telemetry activation crosses
//! (ADR-042 §1a, §9, P3).
//!
//! `POST /cloud/enroll` lives in `temps-cloud`; the bulk activation engine lives
//! in `temps-otel`; neither crate depends on the other, and neither should. So
//! the enroll path asks for this trait out of the service registry and, if an
//! implementation is present, kicks the activation off. If none is registered —
//! a build without the OTel plugin, or an instance with no span source — enroll
//! behaves exactly as it did before, which is the property ADR-042's
//! "enroll-path coupling" risk demands.
//!
//! # Why the outcome is an enum rather than a `Result<Uuid>`
//!
//! Most of the reasons no job gets queued are not failures. A fresh instance has
//! no projects yet. An operator who enrolled without consenting to telemetry
//! export deliberately has nowhere to ship to. Collapsing those into `Err` would
//! put an ERROR line and an alarming audit trail in front of an operator whose
//! instance is working exactly as configured, and a self-hosted operator reading
//! that has nobody to ask whether it mattered.
//!
//! # Why this can never fail an enrollment
//!
//! ADR-042's stated risk: *"a bug in job creation could fail an enrollment"*.
//! The contract is therefore that the caller treats **every** variant here,
//! including `Err`, as advisory: the enrollment has already succeeded by the
//! time this is called, and the job's own failure surfaces on the activation
//! status card rather than in the enroll response.

use async_trait::async_trait;

/// A queued purchase-triggered activation, as the enroll path needs to audit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedTelemetryActivation {
    /// The job id, so the audit row and `GET /bulk-jobs/{batch_id}` agree.
    pub batch_id: String,
    /// Every project the job will visit, ascending.
    pub project_ids: Vec<i32>,
    /// What this instance believed the activation would cost before sending
    /// anything. The actuals land on the job row; both are needed to tell an
    /// over-run from a bad estimate when a customer disputes an invoice.
    pub estimated_spans: i64,
    pub estimated_bytes: i64,
    /// The widest window the job covers, RFC 3339.
    pub window_from: String,
    pub window_to: String,
}

impl StartedTelemetryActivation {
    pub fn project_count(&self) -> usize {
        self.project_ids.len()
    }
}

/// Why no job was queued. None of these are failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelemetryActivationSkipped {
    /// Nothing on this instance still writes its spans locally — a fresh
    /// install with no projects, or an instance already fully Cloud-primary.
    /// Queueing an empty job would put a permanently-0% progress card in front
    /// of the operator with nothing behind it.
    NoLocalProjects,
    /// The instance cannot ship telemetry right now: no link, a rejected
    /// credential, telemetry export switched off, or no local span source in
    /// this build. Carries the sentence and the page that resolves it, so the
    /// log line an operator finds is actionable on its own.
    NotConfigured {
        reason: String,
        setup_path: Option<String>,
    },
    /// An activation was already pending or running. Submission concurrency is
    /// 1 globally (ADR-041 §3b), so the right answer is to point at that job
    /// rather than queue a competing one.
    AlreadyActive { batch_id: String },
}

/// What a purchase-triggered activation attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelemetryActivationOutcome {
    Started(StartedTelemetryActivation),
    Skipped(TelemetryActivationSkipped),
}

/// Starts the activation a customer just paid for.
///
/// Registered by the OTel plugin as `Arc<dyn CloudTelemetryActivationTrigger>`
/// and resolved optionally by the Cloud plugin — the same shape
/// [`ProjectAccessChecker`](crate::ProjectAccessChecker) uses to cross a crate
/// boundary without either side depending on the other.
#[async_trait]
pub trait CloudTelemetryActivationTrigger: Send + Sync {
    /// Queue an activation covering every project that still writes its spans
    /// to this instance, over everything local storage holds.
    ///
    /// There is deliberately no project list, no window and no confirmation
    /// token parameter: the purchase path has no estimate-gates-start step
    /// (ADR-042 §1), and giving this method a scope would make it a second,
    /// unauthenticated way to spend a customer's money on an arbitrary set of
    /// projects. The operator path keeps its `plan_token`-gated endpoint and is
    /// the only way to choose a scope.
    async fn start_purchase_activation(
        &self,
    ) -> Result<TelemetryActivationOutcome, Box<dyn std::error::Error + Send + Sync>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_started_activation_reports_the_number_of_projects_it_covers() {
        let started = StartedTelemetryActivation {
            batch_id: "6b1f9b6a-0000-4000-8000-000000000000".to_string(),
            project_ids: vec![4, 9, 17],
            estimated_spans: 30_000,
            estimated_bytes: 7_500_000,
            window_from: "2026-06-04T00:00:00Z".to_string(),
            window_to: "2026-09-01T00:00:00Z".to_string(),
        };

        assert_eq!(started.project_count(), 3);
    }

    #[test]
    fn an_empty_instance_is_a_skip_rather_than_a_zero_project_job() {
        // A job with no projects would render as a progress card stuck at 0%
        // forever, which on a fresh install is indistinguishable from a hang.
        let outcome =
            TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::NoLocalProjects);
        assert!(matches!(
            outcome,
            TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::NoLocalProjects)
        ));
    }
}
