// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-operation trends and regressions after a deployment (ADR 111, D2).

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::services::operation_trends::{
    self, DeploymentMarker, DeploymentRegressions, OperationChange, OperationTrend,
    TrackingState, TrendPoint,
};
use crate::OtelAppState;

#[derive(OpenApi)]
#[openapi(
    paths(get_operation_trend, get_deployment_regressions),
    components(schemas(
        OperationTrendResponse,
        OperationTrendPointResponse,
        DeploymentMarkerResponse,
        OperationTrackingResponse,
        DeploymentRegressionsResponse,
        OperationChangeResponse
    ))
)]
pub struct OperationTrendsApiDoc;

/// Default trend window, in days.
pub const DEFAULT_TREND_DAYS: i64 = 14;

#[derive(Debug, Deserialize, IntoParams)]
pub struct OperationTrendParams {
    pub project_id: i32,
    pub service_name: String,
    /// The operation (span name).
    pub span_name: String,
    /// Window start (RFC 3339); defaults to 14 days before `end_time`.
    #[param(value_type = Option<String>, format = DateTime)]
    pub start_time: Option<DateTime<Utc>>,
    /// Window end (RFC 3339); defaults to now.
    #[param(value_type = Option<String>, format = DateTime)]
    pub end_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct DeploymentRegressionsParams {
    pub project_id: i32,
    pub deployment_id: i32,
}

/// One hour of one operation.
#[derive(Debug, Serialize, ToSchema)]
pub struct OperationTrendPointResponse {
    #[schema(value_type = String, format = DateTime)]
    pub bucket_start: DateTime<Utc>,
    pub count: i64,
    pub error_count: i64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

impl From<TrendPoint> for OperationTrendPointResponse {
    fn from(point: TrendPoint) -> Self {
        Self {
            bucket_start: point.bucket_start,
            count: point.count,
            error_count: point.error_count,
            avg_ms: point.avg_ms,
            p50_ms: point.p50_ms,
            p95_ms: point.p95_ms,
            p99_ms: point.p99_ms,
        }
    }
}

/// A deployment that went live.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeploymentMarkerResponse {
    pub deployment_id: i32,
    pub slug: String,
    pub environment_id: i32,
    #[schema(value_type = String, format = DateTime)]
    pub live_at: DateTime<Utc>,
}

impl From<DeploymentMarker> for DeploymentMarkerResponse {
    fn from(marker: DeploymentMarker) -> Self {
        Self {
            deployment_id: marker.deployment_id,
            slug: marker.slug,
            environment_id: marker.environment_id,
            live_at: marker.live_at,
        }
    }
}

/// Whether operations of the project are tracked, and how many did not fit.
#[derive(Debug, Serialize, ToSchema)]
pub struct OperationTrackingResponse {
    /// True once the rollup job has run for the project.
    pub configured: bool,
    /// Why there is no history yet, when `configured` is false.
    pub reason: Option<String>,
    /// The console page, relative to the project, that sets up what is
    /// missing.
    pub setup_path: Option<String>,
    /// Hours that start before this have been rolled up.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub rolled_until: Option<DateTime<Utc>>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_run_at: Option<DateTime<Utc>>,
    /// Operations kept in the last hour rolled up.
    pub tracked_operations: i32,
    /// Operations of that hour over the cap, which have no history.
    pub overflow_operations: i32,
    /// Operations kept per project and hour, those with the most total time.
    pub max_tracked_operations: u64,
    /// Days of history kept.
    pub retention_days: i64,
    pub last_error: Option<String>,
}

impl From<Option<TrackingState>> for OperationTrackingResponse {
    fn from(state: Option<TrackingState>) -> Self {
        let configured = state.is_some();
        Self {
            configured,
            reason: (!configured).then(|| {
                "Operation history has not been rolled up yet; the job runs every 10 minutes and keeps one point per complete hour".to_string()
            }),
            setup_path: (!configured).then(|| "traces".to_string()),
            rolled_until: state.as_ref().map(|s| s.rolled_until),
            last_run_at: state.as_ref().map(|s| s.last_run_at),
            tracked_operations: state.as_ref().map_or(0, |s| s.tracked_operations),
            overflow_operations: state.as_ref().map_or(0, |s| s.overflow_operations),
            max_tracked_operations: operation_trends::MAX_TRACKED_OPERATIONS,
            retention_days: operation_trends::RETENTION_DAYS,
            last_error: state.and_then(|s| s.last_error),
        }
    }
}

/// An operation's hourly series, with the deployments in the window.
#[derive(Debug, Serialize, ToSchema)]
pub struct OperationTrendResponse {
    pub service_name: String,
    pub span_name: String,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
    /// Hours with calls, oldest first; hours without calls have no point.
    pub points: Vec<OperationTrendPointResponse>,
    pub deployments: Vec<DeploymentMarkerResponse>,
    pub tracking: OperationTrackingResponse,
}

/// One operation that changed after a deployment.
#[derive(Debug, Serialize, ToSchema)]
pub struct OperationChangeResponse {
    pub service_name: String,
    pub span_name: String,
    /// `regressed` or `improved`.
    pub change: String,
    /// The median hourly p95 in the week before.
    pub baseline_p95_ms: Option<f64>,
    /// The median hourly p95 after.
    pub after_p95_ms: Option<f64>,
    /// `after / baseline - 1`: 0.4 is 40% slower, -0.3 is 30% faster.
    pub change_ratio: Option<f64>,
    pub baseline_hours: usize,
    pub after_hours: usize,
}

impl From<OperationChange> for OperationChangeResponse {
    fn from(change: OperationChange) -> Self {
        Self {
            service_name: change.service_name,
            span_name: change.span_name,
            change: change.comparison.kind.as_str().to_string(),
            baseline_p95_ms: change.comparison.baseline_p95_ms,
            after_p95_ms: change.comparison.after_p95_ms,
            change_ratio: change.comparison.change_ratio,
            baseline_hours: change.comparison.baseline_hours,
            after_hours: change.comparison.after_hours,
        }
    }
}

/// The project's tracked operations compared across one deployment.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeploymentRegressionsResponse {
    pub deployment: DeploymentMarkerResponse,
    #[schema(value_type = String, format = DateTime)]
    pub baseline_start: DateTime<Utc>,
    /// The first complete hour after the deployment went live.
    #[schema(value_type = String, format = DateTime)]
    pub after_start: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub after_end: DateTime<Utc>,
    /// Regressed operations first, largest change first; then improved
    /// ones. At most 50.
    pub changes: Vec<OperationChangeResponse>,
    pub regressed: usize,
    pub improved: usize,
    pub unchanged: usize,
    /// Operations with too little history before the deployment.
    pub insufficient_baseline: usize,
    /// Operations with too few hours after it yet.
    pub insufficient_after: usize,
    /// Complete hours after the deployment needed before it is judged.
    pub min_after_hours: usize,
    pub tracking: OperationTrackingResponse,
}

impl From<DeploymentRegressions> for DeploymentRegressionsResponse {
    fn from(result: DeploymentRegressions) -> Self {
        Self {
            deployment: result.deployment.into(),
            baseline_start: result.baseline_start,
            after_start: result.after_start,
            after_end: result.after_end,
            changes: result.changes.into_iter().map(Into::into).collect(),
            regressed: result.regressed,
            improved: result.improved,
            unchanged: result.unchanged,
            insufficient_baseline: result.insufficient_baseline,
            insufficient_after: result.insufficient_after,
            min_after_hours: operation_trends::MIN_AFTER_HOURS,
            tracking: result.tracking.into(),
        }
    }
}

fn trend_response(
    params: &OperationTrendParams,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    trend: OperationTrend,
) -> OperationTrendResponse {
    OperationTrendResponse {
        service_name: params.service_name.clone(),
        span_name: params.span_name.clone(),
        start_time,
        end_time,
        points: trend.points.into_iter().map(Into::into).collect(),
        deployments: trend.deployments.into_iter().map(Into::into).collect(),
        tracking: trend.tracking.into(),
    }
}

/// Hourly latency of one operation over time
#[utoipa::path(
    get,
    path = "/otel/operations/trend",
    params(OperationTrendParams),
    responses(
        (status = 200, description = "Hourly series of the operation", body = OperationTrendResponse),
        (status = 400, description = "Missing operation, or a window over 30 days"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing otel:read or project access"),
        (status = 500, description = "Internal server error")
    ),
    tag = "OpenTelemetry",
    security(("bearer_auth" = []))
)]
pub async fn get_operation_trend(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<OperationTrendParams>,
) -> Result<Json<OperationTrendResponse>, Problem> {
    permission_guard!(auth, OtelRead);
    project_scope_guard!(auth, params.project_id);
    project_access_guard!(auth, params.project_id, state.project_access_checker);
    let end_time = params.end_time.unwrap_or_else(Utc::now);
    let start_time = params
        .start_time
        .unwrap_or(end_time - Duration::days(DEFAULT_TREND_DAYS));
    let trend = state
        .operation_trends
        .trend(
            params.project_id,
            &params.service_name,
            &params.span_name,
            start_time,
            end_time,
        )
        .await?;
    Ok(Json(trend_response(&params, start_time, end_time, trend)))
}

/// Which operations got slower or faster after a deployment
#[utoipa::path(
    get,
    path = "/otel/operations/regressions",
    params(DeploymentRegressionsParams),
    responses(
        (status = 200, description = "Operations compared across the deployment", body = DeploymentRegressionsResponse),
        (status = 400, description = "The deployment never went live"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing otel:read or project access"),
        (status = 404, description = "No such deployment in the project"),
        (status = 500, description = "Internal server error")
    ),
    tag = "OpenTelemetry",
    security(("bearer_auth" = []))
)]
pub async fn get_deployment_regressions(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<DeploymentRegressionsParams>,
) -> Result<Json<DeploymentRegressionsResponse>, Problem> {
    permission_guard!(auth, OtelRead);
    project_scope_guard!(auth, params.project_id);
    project_access_guard!(auth, params.project_id, state.project_access_checker);
    let result = state
        .operation_trends
        .regressions(params.project_id, params.deployment_id, Utc::now())
        .await?;
    Ok(Json(result.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::operation_trends::{ChangeKind, Comparison};

    #[test]
    fn an_untracked_project_explains_itself_and_links_to_setup() {
        let response = OperationTrackingResponse::from(None);
        assert!(!response.configured);
        assert!(response.reason.as_deref().is_some_and(|r| r.contains("10 minutes")));
        assert_eq!(response.setup_path.as_deref(), Some("traces"));
        assert_eq!(
            response.max_tracked_operations,
            operation_trends::MAX_TRACKED_OPERATIONS
        );
    }

    #[test]
    fn a_change_names_its_direction() {
        let response = OperationChangeResponse::from(OperationChange {
            service_name: "checkout-api".into(),
            span_name: "GET /cart".into(),
            comparison: Comparison {
                kind: ChangeKind::Regressed,
                baseline_p95_ms: Some(100.0),
                after_p95_ms: Some(140.0),
                change_ratio: Some(0.4),
                baseline_hours: 24,
                after_hours: 3,
            },
        });
        assert_eq!(response.change, "regressed");
        assert_eq!(response.change_ratio, Some(0.4));
    }
}
