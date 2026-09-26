// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit trail for the Temps Cloud telemetry backfill (ADR-040 §1).
//!
//! `temps backfill cloud-telemetry` is a write in every sense CLAUDE.md's
//! audit rule means: it ships a project's real span data to a third party, it
//! is paid, and it is one-way — lowering fidelity afterwards does not retract
//! what already left. It is also the *only* write in this feature that is not
//! an HTTP handler, so it has no `RequireAuth`/`RequestMetadata` to derive an
//! actor from.
//!
//! Without this, the sole durable record of a run is the
//! [`crate::services::cloud_backfill_progress`] row, and that row is
//! `UNIQUE (project_id)`: a second run overwrites the first. "Did we ship this
//! customer's real span data, for which window, and when" — the question a
//! compliance-minded operator or an incident responder actually asks — becomes
//! unanswerable the moment anyone re-runs the command. Audit rows accumulate,
//! so they answer it.
//!
//! Two entries per run:
//!
//! - **`CLOUD_TELEMETRY_BACKFILL_STARTED`** — written before a single span is
//!   offered to Cloud, carrying what is about to leave: project, window,
//!   fidelity, allowlist size, source table and the estimated row count.
//! - **`CLOUD_TELEMETRY_BACKFILL_FINISHED`** — written at the terminal state,
//!   carrying what actually left: spans shipped, estimated metered bytes, and
//!   whether the run succeeded or failed (with the reason).
//!
//! Both carry the full context rather than the terminal row referring back to
//! the start row, because an audit reader filtering on one operation type must
//! not have to join to understand what they are looking at.

use serde::Serialize;
use temps_core::{AuditLogger, AuditOperation, DBDateTime};
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;
use tracing::error;

use crate::services::cloud_backfill_progress::truncate_failure_reason;

/// Recorded before anything is offered to Temps Cloud.
pub const CLOUD_TELEMETRY_BACKFILL_STARTED: &str = "CLOUD_TELEMETRY_BACKFILL_STARTED";
/// Recorded once the run reaches a terminal state, either way.
pub const CLOUD_TELEMETRY_BACKFILL_FINISHED: &str = "CLOUD_TELEMETRY_BACKFILL_FINISHED";

/// The actor string for a run. There is no user id: the command is run against
/// the instance's own data directory from an operator's shell, not through an
/// authenticated session, and inventing an actor would be worse than recording
/// honestly that this came from the CLI.
pub const CLOUD_TELEMETRY_BACKFILL_USER_AGENT: &str = "temps-backfill-cloud-telemetry";

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudTelemetryBackfillOutcome {
    Succeeded,
    Failed,
}

/// One audit entry about a Cloud telemetry backfill run.
#[derive(Debug, Clone, Serialize)]
pub struct CloudTelemetryBackfillAudit {
    /// Stored in the audit row's own `operation_type` column, so it is not
    /// repeated inside the JSON payload.
    #[serde(skip_serializing)]
    operation_type: &'static str,
    pub project_id: i32,
    pub window_from: String,
    pub window_to: String,
    /// The fidelity the run projected at — i.e. how much of each span left.
    pub fidelity: CloudTelemetryFidelity,
    /// How many attribute keys were permitted to leave. The keys themselves are
    /// project configuration and readable from the project row; the count is
    /// what makes the entry self-contained without growing unboundedly.
    pub allowlisted_attribute_keys: usize,
    /// Which local table the spans were read from.
    pub source: String,
    /// Rows the pre-flight estimate expected to ship.
    pub spans_estimated: u64,
    /// Rows Temps Cloud actually acknowledged. Absent on the start entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans_shipped: Option<u64>,
    /// This instance's own byte estimate. Cloud's acknowledgement remains the
    /// authoritative billing figure; this is what the operator was shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_metered_bytes: Option<u64>,
    /// Absent on the start entry, always present on the terminal one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CloudTelemetryBackfillOutcome>,
    /// Why the run stopped, bounded exactly like the progress row's
    /// `last_error` — an audit payload is no better a place to republish an
    /// unbounded driver dump than a read endpoint is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

impl CloudTelemetryBackfillAudit {
    /// The entry written before the first span is offered to Cloud.
    pub fn started(
        project_id: i32,
        window_from: DBDateTime,
        window_to: DBDateTime,
        fidelity: CloudTelemetryFidelity,
        allowlisted_attribute_keys: usize,
        source: impl Into<String>,
        spans_estimated: u64,
    ) -> Self {
        Self {
            operation_type: CLOUD_TELEMETRY_BACKFILL_STARTED,
            project_id,
            window_from: window_from.to_rfc3339(),
            window_to: window_to.to_rfc3339(),
            fidelity,
            allowlisted_attribute_keys,
            source: source.into(),
            spans_estimated,
            spans_shipped: None,
            estimated_metered_bytes: None,
            outcome: None,
            failure_reason: None,
        }
    }

    /// The terminal entry for a run that finished every chunk.
    pub fn succeeded(&self, spans_shipped: u64, estimated_metered_bytes: u64) -> Self {
        Self {
            operation_type: CLOUD_TELEMETRY_BACKFILL_FINISHED,
            spans_shipped: Some(spans_shipped),
            estimated_metered_bytes: Some(estimated_metered_bytes),
            outcome: Some(CloudTelemetryBackfillOutcome::Succeeded),
            failure_reason: None,
            ..self.clone()
        }
    }

    /// The terminal entry for a run that stopped partway.
    ///
    /// `spans_shipped` is what had already left before the failure — the number
    /// that matters, because those bytes are gone and billed regardless.
    pub fn failed(
        &self,
        spans_shipped: u64,
        estimated_metered_bytes: u64,
        reason: impl AsRef<str>,
    ) -> Self {
        Self {
            operation_type: CLOUD_TELEMETRY_BACKFILL_FINISHED,
            spans_shipped: Some(spans_shipped),
            estimated_metered_bytes: Some(estimated_metered_bytes),
            outcome: Some(CloudTelemetryBackfillOutcome::Failed),
            failure_reason: Some(truncate_failure_reason(reason.as_ref())),
            ..self.clone()
        }
    }
}

impl AuditOperation for CloudTelemetryBackfillAudit {
    fn operation_type(&self) -> String {
        self.operation_type.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        None
    }

    fn ip_address(&self) -> Option<String> {
        None
    }

    fn user_agent(&self) -> &str {
        CLOUD_TELEMETRY_BACKFILL_USER_AGENT
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
}

/// Persist one entry, logging — never propagating — a failure.
///
/// Same rule as the progress record and as every audit call site in this
/// codebase: audit is bookkeeping *about* the transfer, not part of it, and
/// aborting a paid, half-finished egress because a log row would not insert is
/// strictly worse than losing the row. The `error!` is loud enough that an
/// operator who later finds no audit trail can see why.
pub async fn record_backfill_audit(logger: &dyn AuditLogger, event: &CloudTelemetryBackfillAudit) {
    if let Err(error) = logger.create_audit_log(event).await {
        error!(
            operation = event.operation_type,
            project_id = event.project_id,
            %error,
            "Could not record the Cloud telemetry backfill audit entry; the transfer is \
             unaffected but this run will be missing from the audit log"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn window() -> (DBDateTime, DBDateTime) {
        (
            chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                .expect("fixed timestamp parses")
                .with_timezone(&chrono::Utc),
            chrono::DateTime::parse_from_rfc3339("2026-08-31T00:00:00Z")
                .expect("fixed timestamp parses")
                .with_timezone(&chrono::Utc),
        )
    }

    /// `serde::Serialize` is also in scope here via `use super::*`, so the
    /// audit trait's method needs naming explicitly.
    fn payload(event: &CloudTelemetryBackfillAudit) -> String {
        AuditOperation::serialize(event).expect("entry must serialize")
    }

    fn started() -> CloudTelemetryBackfillAudit {
        let (from, to) = window();
        CloudTelemetryBackfillAudit::started(
            7,
            from,
            to,
            CloudTelemetryFidelity::Queryable,
            2,
            "PostgreSQL `otel_spans`",
            5_000,
        )
    }

    #[derive(Default)]
    struct RecordingLogger {
        recorded: Mutex<Vec<(String, String)>>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingLogger {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            if self.fail {
                return Err(anyhow::anyhow!("audit table unavailable"));
            }
            self.recorded
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((operation.operation_type(), operation.serialize()?));
            Ok(())
        }
    }

    #[test]
    fn the_start_entry_records_what_is_about_to_leave() {
        // This is the entry that answers "which window, at what fidelity, with
        // how many attribute keys" for a run whose progress row was later
        // overwritten by a second run.
        let payload = payload(&started());

        assert!(payload.contains(r#""project_id":7"#), "{payload}");
        assert!(
            payload.contains(r#""window_from":"2026-08-01T00:00:00+00:00""#),
            "{payload}"
        );
        assert!(
            payload.contains(r#""window_to":"2026-08-31T00:00:00+00:00""#),
            "{payload}"
        );
        assert!(payload.contains(r#""fidelity":"queryable""#), "{payload}");
        assert!(
            payload.contains(r#""allowlisted_attribute_keys":2"#),
            "{payload}"
        );
        assert!(payload.contains(r#""spans_estimated":5000"#), "{payload}");
        // Nothing has shipped yet, so claiming a count would be a lie.
        assert!(!payload.contains("spans_shipped"), "{payload}");
        assert!(!payload.contains("outcome"), "{payload}");
    }

    #[test]
    fn the_terminal_entry_keeps_the_windows_context_and_adds_the_result() {
        // A reader filtering on the terminal operation type must not have to
        // join back to the start entry to know what was sent.
        let event = started().succeeded(4_987, 1_250_000);

        assert_eq!(event.operation_type(), CLOUD_TELEMETRY_BACKFILL_FINISHED);
        let payload = payload(&event);
        assert!(
            payload.contains(r#""window_from":"2026-08-01T00:00:00+00:00""#),
            "{payload}"
        );
        assert!(payload.contains(r#""fidelity":"queryable""#), "{payload}");
        assert!(payload.contains(r#""spans_shipped":4987"#), "{payload}");
        assert!(
            payload.contains(r#""estimated_metered_bytes":1250000"#),
            "{payload}"
        );
        assert!(payload.contains(r#""outcome":"succeeded""#), "{payload}");
        assert!(!payload.contains("failure_reason"), "{payload}");
    }

    #[test]
    fn a_failed_run_records_the_spans_that_already_left() {
        // Those bytes are gone and billed; an audit entry that reported zero
        // because the run failed would be actively misleading.
        let event = started().failed(1_200, 300_000, "Temps Cloud refused the batch");

        let payload = payload(&event);
        assert!(payload.contains(r#""spans_shipped":1200"#), "{payload}");
        assert!(payload.contains(r#""outcome":"failed""#), "{payload}");
        assert!(
            payload.contains(r#""failure_reason":"Temps Cloud refused the batch""#),
            "{payload}"
        );
    }

    #[test]
    fn a_failure_reason_is_bounded_before_it_reaches_the_audit_payload() {
        let event = started().failed(0, 0, "x".repeat(10_000));

        let reason = event.failure_reason.expect("a failed run records a reason");
        assert!(reason.chars().count() < 400, "{}", reason.chars().count());
        assert_eq!(reason, truncate_failure_reason(&"x".repeat(10_000)));
    }

    #[test]
    fn the_two_entries_use_distinct_operation_types() {
        // They are filtered separately: "what was attempted" and "what
        // happened" are different audit questions.
        assert_eq!(started().operation_type(), CLOUD_TELEMETRY_BACKFILL_STARTED);
        assert_ne!(
            CLOUD_TELEMETRY_BACKFILL_STARTED,
            CLOUD_TELEMETRY_BACKFILL_FINISHED
        );
    }

    #[test]
    fn a_run_has_no_user_or_ip_because_it_has_no_request() {
        let event = started();
        assert_eq!(event.user_id(), None);
        assert_eq!(event.ip_address(), None);
        assert_eq!(event.user_agent(), CLOUD_TELEMETRY_BACKFILL_USER_AGENT);
    }

    #[tokio::test]
    async fn recording_persists_both_entries_in_order() {
        let logger = Arc::new(RecordingLogger::default());
        let start = started();

        record_backfill_audit(logger.as_ref(), &start).await;
        record_backfill_audit(logger.as_ref(), &start.succeeded(10, 20)).await;

        let recorded = logger
            .recorded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, CLOUD_TELEMETRY_BACKFILL_STARTED);
        assert_eq!(recorded[1].0, CLOUD_TELEMETRY_BACKFILL_FINISHED);
    }

    #[tokio::test]
    async fn a_failed_audit_write_does_not_abort_the_caller() {
        // A half-finished, already-paid-for egress must not be aborted because
        // a bookkeeping row would not insert.
        let logger = RecordingLogger {
            fail: true,
            ..Default::default()
        };

        record_backfill_audit(&logger, &started()).await;
    }
}
