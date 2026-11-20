// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether performance issues are being detected for a project (ADR 111, D2).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use utoipa::{OpenApi, ToSchema};

use super::types::AppState;
use crate::services::performance::{self, detectors};

#[derive(OpenApi)]
#[openapi(
    paths(get_performance_issue_status),
    components(schemas(PerformanceIssueStatusResponse, PerformanceDetectorResponse)),
    tags((name = "error-tracking", description = "Performance issues"))
)]
pub struct PerformanceApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/projects/{project_id}/performance-issues/status",
        get(get_performance_issue_status),
    )
}

/// One detector and what it looks for.
#[derive(Debug, Serialize, ToSchema)]
pub struct PerformanceDetectorResponse {
    /// `n_plus_one_query`, `slow_db_query`, `sequential_http` or
    /// `oversized_payload`.
    pub key: String,
    /// The issue type it files, e.g. `N+1 Query`.
    pub title: String,
    pub description: String,
}

/// Whether traces of the project are checked for performance issues, and
/// what the last check did.
#[derive(Debug, Serialize, ToSchema)]
pub struct PerformanceIssueStatusResponse {
    /// True once a check has read at least one trace of the project.
    pub configured: bool,
    /// Why nothing is detected yet, when `configured` is false.
    pub reason: Option<String>,
    /// The console page, relative to the project, that sets up what is
    /// missing.
    pub setup_path: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Traces started before this have been considered.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub scanned_until: Option<chrono::DateTime<chrono::Utc>>,
    pub traces_scanned: i64,
    pub issues_found: i64,
    /// The last check's failure, if it failed.
    pub last_error: Option<String>,
    /// Traces checked per project per check, the slowest first.
    pub traces_per_check: u64,
    /// Minutes between checks.
    pub check_interval_minutes: u64,
    pub detectors: Vec<PerformanceDetectorResponse>,
}

fn detector_list() -> Vec<PerformanceDetectorResponse> {
    use detectors::DetectorKind::*;
    [
        (
            NPlusOneQuery,
            format!(
                "The same database query {} or more times under one parent span",
                detectors::N_PLUS_ONE_MIN_REPEATS
            ),
        ),
        (
            SlowDbQuery,
            format!(
                "A database span of {:.0} ms or more",
                detectors::SLOW_DB_SPAN_MS
            ),
        ),
        (
            SequentialHttp,
            format!(
                "{} or more HTTP calls of {:.0} ms or more, one after another, that could run in parallel",
                detectors::SEQUENTIAL_HTTP_MIN_CALLS,
                detectors::SEQUENTIAL_HTTP_MIN_CALL_MS
            ),
        ),
        (
            OversizedPayload,
            format!(
                "A request or response body of {} MB or more",
                detectors::OVERSIZED_PAYLOAD_BYTES / 1_000_000
            ),
        ),
    ]
    .into_iter()
    .map(|(kind, description)| PerformanceDetectorResponse {
        key: kind.key().to_string(),
        title: kind.title().to_string(),
        description,
    })
    .collect()
}

/// Whether performance issues are detected for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/performance-issues/status",
    responses(
        (status = 200, description = "Detection status", body = PerformanceIssueStatusResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing error_tracking:read or project access"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn get_performance_issue_status(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
) -> Result<Json<PerformanceIssueStatusResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let scan = state
        .error_tracking_service
        .performance_scan_state(project_id)
        .await?;
    let configured = scan.as_ref().is_some_and(|scan| scan.traces_scanned > 0);
    let reason = (!configured).then(|| match &scan {
        None => "Traces of this project have not been checked yet; checks run every few minutes once the project sends traces".to_string(),
        Some(_) => "No traces have been received from this project. Send traces with OpenTelemetry or a Sentry SDK with tracing enabled".to_string(),
    });
    Ok(Json(PerformanceIssueStatusResponse {
        configured,
        reason,
        setup_path: (!configured).then(|| "traces".to_string()),
        last_run_at: scan.as_ref().map(|scan| scan.last_run_at),
        scanned_until: scan.as_ref().map(|scan| scan.scanned_until),
        traces_scanned: scan.as_ref().map_or(0, |scan| scan.traces_scanned),
        issues_found: scan.as_ref().map_or(0, |scan| scan.issues_found),
        last_error: scan.and_then(|scan| scan.last_error),
        traces_per_check: performance::TRACES_PER_PROJECT,
        check_interval_minutes: performance::TICK.as_secs() / 60,
        detectors: detector_list(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::group_workflow_handler::tests::{
        app_state, test_user, RecordingAuditLogger,
    };
    use temps_authz::Role;

    #[test]
    fn every_detector_is_described() {
        let list = detector_list();
        assert_eq!(list.len(), 4);
        assert!(list
            .iter()
            .any(|d| d.key == "n_plus_one_query" && d.description.contains("5 or more")));
    }

    #[tokio::test]
    async fn a_project_never_checked_explains_why_and_links_to_setup() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results::<sea_orm::MockRow, _, _>([Vec::<sea_orm::MockRow>::new()])
                .into_connection(),
        );
        let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));
        let status = get_performance_issue_status(
            State(state),
            RequireAuth(test_user(3, Role::Reader)),
            Path(1),
        )
        .await
        .expect("status")
        .0;
        assert!(!status.configured);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("not been checked")));
        assert_eq!(status.setup_path.as_deref(), Some("traces"));
        assert_eq!(status.traces_scanned, 0);
    }
}
