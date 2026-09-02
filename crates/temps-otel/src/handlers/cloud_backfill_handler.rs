// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-only status of the Temps Cloud telemetry backfill (ADR-040 §1).
//!
//! The backfill itself is deliberately a CLI/operator action run outside
//! `temps serve` — the ADR is explicit that it must not contend with live
//! ingest, and shipping data to a paid destination should be a deliberate act,
//! not a button. But "the Console cannot trigger it" must not become "the
//! Console cannot see it": an operator who never ran the command has no way to
//! learn it exists, and one whose run died an hour ago has no way to find out.
//!
//! So this endpoint is read-only by design and always answers, in every state:
//!
//! - **`not_started`** — carries the exact command to run. Onboarding, not an
//!   error, and never hidden.
//! - **`running`** — spans processed / total / percent, plus `updated_at` so
//!   the client can tell a live run from a stalled one.
//! - **`completed`** — when.
//! - **`failed`** — the verbatim reason.
//!
//! It also reports the project's current `cloud_telemetry_fidelity`, because
//! "backfill never run" and "this project has not opted in, so a backfill would
//! be refused" are different problems with different fixes, and a status field
//! alone cannot distinguish them.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

use crate::services::cloud_backfill_progress::{
    percent_complete, truncate_failure_reason, CloudBackfillProgressError,
};
use crate::OtelAppState;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::ProblemDetails;
use temps_entities::cloud_telemetry_backfills::CloudTelemetryBackfillStatus;
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;

/// Console path that owns the per-project fidelity opt-in.
const FIDELITY_SETUP_PATH: &str = "/settings/cloud";

#[derive(Debug, Serialize, ToSchema)]
pub struct CloudBackfillStatusResponse {
    pub project_id: i32,
    /// `not_started` when no backfill has ever run for this project.
    pub status: CloudTelemetryBackfillStatus,
    /// Current per-project egress fidelity. A `metered` project cannot be
    /// backfilled at all — the CLI refuses — so the client must be able to
    /// render "not set up" rather than "not started".
    pub fidelity: CloudTelemetryFidelity,
    /// Whether a backfill would be accepted right now. False means the project
    /// is still at `metered` fidelity.
    pub backfill_available: bool,
    pub spans_processed: i64,
    pub spans_total: i64,
    /// `spans_processed / spans_total`, clamped to 0–100. `None` when the total
    /// is unknown or zero — an empty window is not "0% done".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_complete: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub window_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub window_to: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Bumped on every progress write. A `running` status whose `updated_at` is
    /// far in the past is a stalled run, and the client should say so instead
    /// of showing a spinner forever.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Reason the last run stopped, when `status` is `failed`. Verbatim, but
    /// length-bounded: it originates as raw driver/server text and is readable
    /// by every project member with `OtelRead`, not only whoever ran the
    /// command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The exact command an operator should run. Always present, in every
    /// state, so the capability is discoverable from the Console even though
    /// the Console deliberately cannot trigger it.
    pub command: String,
    /// Where to go to raise fidelity, when `backfill_available` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,
}

impl CloudBackfillStatusResponse {
    /// The command line for `project_id`, with a placeholder window an
    /// operator can edit. Never abbreviated: a half-shown command is a command
    /// the reader has to go and look up.
    pub fn command_for(project_id: i32) -> String {
        format!(
            "temps backfill cloud-telemetry --project {project_id} \
             --from <RFC3339> --to <RFC3339> --dry-run"
        )
    }
}

impl From<CloudBackfillProgressError> for Problem {
    fn from(error: CloudBackfillProgressError) -> Self {
        match error {
            CloudBackfillProgressError::Read { .. } | CloudBackfillProgressError::Write { .. } => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Cloud Backfill Progress Unavailable")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Status of the most recent Temps Cloud telemetry backfill for a project.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/cloud-telemetry/backfill/{project_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Backfill status", body = CloudBackfillStatusResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_cloud_backfill_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let fidelity = match state.otel_service.cloud_policy_cache() {
        Some(cache) => cache.policy_for(project_id).await.fidelity,
        // No resolver wired: the mirror runs at `metered`, so report that
        // rather than implying an opt-in the ingest path is not honouring.
        None => CloudTelemetryFidelity::Metered,
    };
    let progress = state.cloud_backfill_progress.get(project_id).await?;

    Ok(Json(build_response(project_id, fidelity, progress)))
}

/// Assemble the response, materialising "no row" as `not_started`.
///
/// Split out so the state mapping is unit-testable without a database.
fn build_response(
    project_id: i32,
    fidelity: CloudTelemetryFidelity,
    progress: Option<temps_entities::cloud_telemetry_backfills::Model>,
) -> CloudBackfillStatusResponse {
    let backfill_available = fidelity.is_queryable();
    let setup_path = if backfill_available {
        None
    } else {
        Some(FIDELITY_SETUP_PATH.to_string())
    };
    let command = CloudBackfillStatusResponse::command_for(project_id);

    match progress {
        Some(row) => CloudBackfillStatusResponse {
            project_id,
            status: row.status,
            fidelity,
            backfill_available,
            spans_processed: row.spans_processed,
            spans_total: row.spans_total,
            percent_complete: percent_complete(row.spans_processed, row.spans_total),
            window_from: row.window_from,
            window_to: row.window_to,
            started_at: row.started_at,
            updated_at: Some(row.updated_at),
            completed_at: row.completed_at,
            // Only surface a stored reason while the run is actually failed;
            // a completed run must never render last time's error. Bounded
            // again on the way out: the writer already caps it, and this also
            // covers a row written by anything else (an older build, a manual
            // `UPDATE`) without trusting the column's contents.
            last_error: match row.status {
                CloudTelemetryBackfillStatus::Failed => {
                    row.last_error.as_deref().map(truncate_failure_reason)
                }
                _ => None,
            },
            command,
            setup_path,
        },
        None => CloudBackfillStatusResponse {
            project_id,
            status: CloudTelemetryBackfillStatus::NotStarted,
            fidelity,
            backfill_available,
            spans_processed: 0,
            spans_total: 0,
            percent_complete: None,
            window_from: None,
            window_to: None,
            started_at: None,
            updated_at: None,
            completed_at: None,
            last_error: None,
            command,
            setup_path,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::cloud_telemetry_backfills::Model as BackfillProgress;

    fn row(
        status: CloudTelemetryBackfillStatus,
        processed: i64,
        total: i64,
        last_error: Option<&str>,
    ) -> BackfillProgress {
        let now = chrono::Utc::now();
        BackfillProgress {
            id: 1,
            project_id: 7,
            status,
            spans_processed: processed,
            spans_total: total,
            window_from: Some(now - chrono::Duration::days(30)),
            window_to: Some(now),
            started_at: Some(now - chrono::Duration::minutes(5)),
            updated_at: now,
            completed_at: if status == CloudTelemetryBackfillStatus::Completed {
                Some(now)
            } else {
                None
            },
            last_error: last_error.map(str::to_string),
            // A run driven by `temps backfill cloud-telemetry` — the case this
            // response shape was built for — has no bulk activation job.
            bulk_job_id: None,
        }
    }

    #[test]
    fn no_row_reports_not_started_rather_than_an_error() {
        // A 404 here would be indistinguishable from "this feature does not
        // exist", which is the exact ambiguity CLAUDE.md's onboarding rule
        // forbids.
        let response = build_response(7, CloudTelemetryFidelity::Queryable, None);

        assert_eq!(response.status, CloudTelemetryBackfillStatus::NotStarted);
        assert_eq!(response.spans_processed, 0);
        assert_eq!(response.percent_complete, None);
        assert_eq!(response.last_error, None);
    }

    #[test]
    fn the_command_is_present_in_every_state_including_not_started() {
        // The Console cannot run the backfill, so the command is the only way
        // an operator learns the capability exists.
        for progress in [
            None,
            Some(row(CloudTelemetryBackfillStatus::Running, 10, 100, None)),
            Some(row(CloudTelemetryBackfillStatus::Completed, 100, 100, None)),
            Some(row(
                CloudTelemetryBackfillStatus::Failed,
                10,
                100,
                Some("Cloud refused the batch"),
            )),
        ] {
            let response = build_response(7, CloudTelemetryFidelity::Queryable, progress);
            assert!(
                response.command.contains("temps backfill cloud-telemetry"),
                "{}",
                response.command
            );
            assert!(
                response.command.contains("--project 7"),
                "{}",
                response.command
            );
        }
    }

    #[test]
    fn a_metered_project_reports_unavailable_with_a_setup_path() {
        // "Not opted in" is a configuration state, not a failure, and it needs
        // a link to the page that fixes it.
        let response = build_response(7, CloudTelemetryFidelity::Metered, None);

        assert!(!response.backfill_available);
        assert_eq!(response.fidelity, CloudTelemetryFidelity::Metered);
        assert_eq!(response.setup_path.as_deref(), Some(FIDELITY_SETUP_PATH));
        assert_eq!(response.status, CloudTelemetryBackfillStatus::NotStarted);
    }

    #[test]
    fn a_queryable_project_carries_no_setup_path() {
        let response = build_response(7, CloudTelemetryFidelity::Queryable, None);

        assert!(response.backfill_available);
        assert_eq!(response.setup_path, None);
    }

    #[test]
    fn a_running_backfill_reports_progress_and_a_liveness_timestamp() {
        let response = build_response(
            7,
            CloudTelemetryFidelity::Queryable,
            Some(row(CloudTelemetryBackfillStatus::Running, 25, 100, None)),
        );

        assert_eq!(response.status, CloudTelemetryBackfillStatus::Running);
        assert_eq!(response.spans_processed, 25);
        assert_eq!(response.spans_total, 100);
        assert_eq!(response.percent_complete, Some(25.0));
        assert!(
            response.updated_at.is_some(),
            "the client needs a liveness signal to tell a live run from a dead one"
        );
        assert_eq!(response.completed_at, None);
    }

    #[test]
    fn a_failed_backfill_surfaces_the_reason_verbatim() {
        let response = build_response(
            7,
            CloudTelemetryFidelity::Queryable,
            Some(row(
                CloudTelemetryBackfillStatus::Failed,
                25,
                100,
                Some("Temps Cloud did not accept a batch of 500 span(s)"),
            )),
        );

        assert_eq!(response.status, CloudTelemetryBackfillStatus::Failed);
        assert_eq!(
            response.last_error.as_deref(),
            Some("Temps Cloud did not accept a batch of 500 span(s)")
        );
    }

    #[test]
    fn an_oversized_stored_error_is_bounded_before_it_is_served() {
        // This field reaches every project member with `OtelRead`, and its
        // source is raw `DbErr`/ClickHouse text. A row that somehow holds an
        // unbounded message must not be republished in full.
        let oversized = format!("Cloud refused the batch: {}", "x".repeat(10_000));
        let response = build_response(
            7,
            CloudTelemetryFidelity::Queryable,
            Some(row(
                CloudTelemetryBackfillStatus::Failed,
                25,
                100,
                Some(&oversized),
            )),
        );

        let served = response.last_error.expect("a failed run reports a reason");
        assert!(served.chars().count() < oversized.chars().count());
        assert!(served.starts_with("Cloud refused the batch: xxx"));
    }

    #[test]
    fn a_completed_backfill_never_renders_a_previous_runs_error() {
        // The row keeps whatever was last written; the response must not show
        // it once the state moved on, or a healthy run reads as broken.
        let response = build_response(
            7,
            CloudTelemetryFidelity::Queryable,
            Some(row(
                CloudTelemetryBackfillStatus::Completed,
                100,
                100,
                Some("stale failure from the previous attempt"),
            )),
        );

        assert_eq!(response.status, CloudTelemetryBackfillStatus::Completed);
        assert_eq!(response.last_error, None);
        assert_eq!(response.percent_complete, Some(100.0));
        assert!(response.completed_at.is_some());
    }

    #[test]
    fn an_empty_window_reports_no_percentage_rather_than_zero() {
        let response = build_response(
            7,
            CloudTelemetryFidelity::Queryable,
            Some(row(CloudTelemetryBackfillStatus::Completed, 0, 0, None)),
        );

        assert_eq!(
            response.percent_complete, None,
            "0% would read as stuck; the run simply had nothing to send"
        );
    }

    #[test]
    fn progress_errors_map_to_500_with_the_project_in_the_detail() {
        let problem: Problem = CloudBackfillProgressError::Read {
            project_id: 7,
            source: sea_orm::DbErr::Custom("connection reset".into()),
        }
        .into();

        let body = format!("{problem:?}");
        assert!(body.contains("500"), "{body}");
        assert!(body.contains("project 7"), "{body}");
    }
}
