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

use crate::services::operation_trends::breakdown::{AttributeSlice, BreakdownGroup};
use crate::services::operation_trends::page_backend::{BackendRequest, PageBackend, PageVitals};
use crate::services::operation_trends::{
    self, BreakdownSample, DeploymentMarker, OperationBreakdown, DeploymentRegressions, OperationChange, OperationTrend,
    TrackingState, TrendPoint,
};
use crate::OtelAppState;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_operation_trend,
        get_deployment_regressions,
        get_operation_breakdown,
        get_page_backend
    ),
    components(schemas(
        PageBackendResponse,
        PageVitalsResponse,
        ServiceRequestsResponse,
        BackendRequestResponse,
        OperationBreakdownResponse,
        BreakdownGroupResponse,
        AttributeSliceResponse,
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
pub struct OperationBreakdownParams {
    pub project_id: i32,
    pub service_name: String,
    /// The operation (span name).
    pub span_name: String,
    /// Window start (RFC 3339); defaults to 24 hours before `end_time`.
    #[param(value_type = Option<String>, format = DateTime)]
    pub start_time: Option<DateTime<Utc>>,
    /// Window end (RFC 3339); defaults to now.
    #[param(value_type = Option<String>, format = DateTime)]
    pub end_time: Option<DateTime<Utc>>,
    /// `recent` (default): the latest calls. `slow`: the latest calls at or
    /// over the window's p95.
    pub sample: Option<String>,
    /// Also split the calls by the value of this attribute of the
    /// operation's own spans, e.g. `http.route` or `tenant.id`.
    pub attribute: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct PageBackendParams {
    pub project_id: i32,
    /// The page path, as the analytics SDK records it, e.g. `/checkout`.
    pub path: String,
    /// Window start (RFC 3339); defaults to 24 hours before `end_time`.
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

/// One downstream call inside the operation.
#[derive(Debug, Serialize, ToSchema)]
pub struct BreakdownGroupResponse {
    /// Empty for the summed remainder.
    pub service_name: String,
    pub span_name: String,
    /// `database`, `http`, `messaging`, `rpc`, `client`, `server`,
    /// `internal`, or `other` for the summed remainder.
    pub category: String,
    /// Exclusive time, summed over the sample.
    pub total_ms: f64,
    /// `total_ms` over the operation's total time. Parallel calls can add
    /// up to more than 1.
    pub share: f64,
    pub calls: u64,
    /// Calls per sampled call of the operation.
    pub calls_per_instance: f64,
    /// Exclusive time per call.
    pub avg_ms: f64,
}

/// One value of the requested attribute.
#[derive(Debug, Serialize, ToSchema)]
pub struct AttributeSliceResponse {
    /// Null for calls without the attribute.
    pub value: Option<String>,
    pub calls: u64,
    pub total_ms: f64,
    pub share: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
}

/// Where the time of an operation goes, from a sample of its calls.
#[derive(Debug, Serialize, ToSchema)]
pub struct OperationBreakdownResponse {
    pub service_name: String,
    pub span_name: String,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
    /// `recent` or `slow`.
    pub sample: String,
    /// The p95 a `slow` sample started from.
    pub slow_threshold_ms: Option<f64>,
    /// Traces read; at most `max_traces`.
    pub traces: usize,
    pub max_traces: usize,
    /// Calls of the operation found in them.
    pub instances: u64,
    pub total_ms: f64,
    /// The operation's own exclusive time.
    pub self_ms: f64,
    pub self_share: f64,
    /// Largest exclusive time first, at most 25 (the rest summed).
    pub groups: Vec<BreakdownGroupResponse>,
    /// The attribute the calls were split by, if any.
    pub attribute: Option<String>,
    /// Largest total time first, at most 20 values (the rest summed).
    pub by_attribute: Vec<AttributeSliceResponse>,
}

/// The 75th percentiles of a page's web vitals.
#[derive(Debug, Serialize, ToSchema)]
pub struct PageVitalsResponse {
    pub page_loads: i64,
    pub ttfb_p75_ms: Option<f64>,
    pub fcp_p75_ms: Option<f64>,
    pub lcp_p75_ms: Option<f64>,
    pub inp_p75_ms: Option<f64>,
}

impl From<PageVitals> for PageVitalsResponse {
    fn from(v: PageVitals) -> Self {
        Self {
            page_loads: v.page_loads,
            ttfb_p75_ms: v.ttfb_p75_ms,
            fcp_p75_ms: v.fcp_p75_ms,
            lcp_p75_ms: v.lcp_p75_ms,
            inp_p75_ms: v.inp_p75_ms,
        }
    }
}

/// One backend request that served the page.
#[derive(Debug, Serialize, ToSchema)]
pub struct BackendRequestResponse {
    pub trace_id: String,
    pub span_name: String,
    pub service_name: String,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    pub duration_ms: f64,
    pub error: bool,
}

impl From<BackendRequest> for BackendRequestResponse {
    fn from(r: BackendRequest) -> Self {
        Self {
            trace_id: r.trace_id,
            span_name: r.span_name,
            service_name: r.service_name,
            start_time: r.start_time,
            duration_ms: r.duration_ms,
            error: r.error,
        }
    }
}

/// How many of a page's backend requests one service served.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceRequestsResponse {
    pub service_name: String,
    pub requests: usize,
}

/// A page's web vitals next to the backend requests for its path.
#[derive(Debug, Serialize, ToSchema)]
pub struct PageBackendResponse {
    pub path: String,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
    pub vitals: PageVitalsResponse,
    /// True when backend requests for the path were found.
    pub configured: bool,
    /// Why none were, when `configured` is false.
    pub reason: Option<String>,
    /// The console page, relative to the project, that sets up what is
    /// missing.
    pub setup_path: Option<String>,
    /// Backend requests found; at most 500 per attribute are read.
    pub requests: usize,
    pub errors: usize,
    pub p50_ms: Option<f64>,
    pub p75_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    /// Backend p75 over the page's p75 time to first byte: the part of the
    /// wait the backend explains. Null without both.
    pub backend_share_of_ttfb: Option<f64>,
    /// Services that served the path, most requests first.
    pub services: Vec<ServiceRequestsResponse>,
    /// Slowest first, at most 10, each linking to its trace.
    pub slowest: Vec<BackendRequestResponse>,
}

fn page_backend_response(
    path: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    result: PageBackend,
) -> PageBackendResponse {
    let configured = result.requests > 0;
    let backend_share_of_ttfb = match (result.p75_ms, result.vitals.ttfb_p75_ms) {
        (Some(backend), Some(ttfb)) if ttfb > 0.0 => Some(backend / ttfb),
        _ => None,
    };
    PageBackendResponse {
        reason: (!configured).then(|| {
            format!(
                "No backend request of this project for {path} was traced in this window. Server spans need the url.path (or http.target) attribute, which OpenTelemetry and Sentry HTTP instrumentations set; a static page has no backend request."
            )
        }),
        setup_path: (!configured).then(|| "traces".to_string()),
        configured,
        path,
        start_time,
        end_time,
        vitals: result.vitals.into(),
        requests: result.requests,
        errors: result.errors,
        p50_ms: result.p50_ms,
        p75_ms: result.p75_ms,
        p95_ms: result.p95_ms,
        backend_share_of_ttfb,
        services: result
            .services
            .into_iter()
            .map(|(service_name, requests)| ServiceRequestsResponse {
                service_name,
                requests,
            })
            .collect(),
        slowest: result.slowest.into_iter().map(Into::into).collect(),
    }
}

fn share(part: f64, total: f64) -> f64 {
    if total > 0.0 {
        part / total
    } else {
        0.0
    }
}

fn breakdown_response(
    params: &OperationBreakdownParams,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    result: OperationBreakdown,
) -> OperationBreakdownResponse {
    let b = result.breakdown;
    let total = b.total_ms;
    let instances = b.instances;
    OperationBreakdownResponse {
        service_name: params.service_name.clone(),
        span_name: params.span_name.clone(),
        start_time,
        end_time,
        sample: result.sample.as_str().to_string(),
        slow_threshold_ms: result.slow_threshold_ms,
        traces: b.traces,
        max_traces: operation_trends::BREAKDOWN_TRACES,
        instances,
        total_ms: total,
        self_ms: b.self_ms,
        self_share: share(b.self_ms, total),
        groups: b
            .groups
            .into_iter()
            .map(|g: BreakdownGroup| BreakdownGroupResponse {
                share: share(g.total_ms, total),
                calls_per_instance: share(g.calls as f64, instances as f64),
                avg_ms: share(g.total_ms, g.calls as f64),
                service_name: g.service_name,
                span_name: g.span_name,
                category: g.category,
                total_ms: g.total_ms,
                calls: g.calls,
            })
            .collect(),
        attribute: params.attribute.clone(),
        by_attribute: b
            .by_attribute
            .into_iter()
            .map(|s: AttributeSlice| AttributeSliceResponse {
                share: share(s.total_ms, total),
                value: s.value,
                calls: s.calls,
                total_ms: s.total_ms,
                avg_ms: s.avg_ms,
                max_ms: s.max_ms,
            })
            .collect(),
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

/// Where the time inside one operation goes
#[utoipa::path(
    get,
    path = "/otel/operations/breakdown",
    params(OperationBreakdownParams),
    responses(
        (status = 200, description = "Downstream calls by exclusive time", body = OperationBreakdownResponse),
        (status = 400, description = "Missing operation, unknown sample, or a window over 7 days"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing otel:read or project access"),
        (status = 500, description = "Internal server error")
    ),
    tag = "OpenTelemetry",
    security(("bearer_auth" = []))
)]
pub async fn get_operation_breakdown(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<OperationBreakdownParams>,
) -> Result<Json<OperationBreakdownResponse>, Problem> {
    permission_guard!(auth, OtelRead);
    project_scope_guard!(auth, params.project_id);
    project_access_guard!(auth, params.project_id, state.project_access_checker);
    let sample = match params.sample.as_deref() {
        None => BreakdownSample::Recent,
        Some(value) => BreakdownSample::parse(value).ok_or_else(|| {
            temps_core::error_builder::bad_request()
                .title("Invalid sample")
                .detail(format!("sample must be recent or slow, got {value}"))
                .build()
        })?,
    };
    let end_time = params.end_time.unwrap_or_else(Utc::now);
    let start_time = params
        .start_time
        .unwrap_or(end_time - Duration::hours(24));
    let attribute = params.attribute.as_deref().filter(|a| !a.is_empty());
    let result = state
        .operation_trends
        .breakdown(
            params.project_id,
            &params.service_name,
            &params.span_name,
            start_time,
            end_time,
            sample,
            attribute,
        )
        .await?;
    Ok(Json(breakdown_response(&params, start_time, end_time, result)))
}

/// A page's web vitals next to the backend requests that served it
#[utoipa::path(
    get,
    path = "/otel/pages/backend",
    params(PageBackendParams),
    responses(
        (status = 200, description = "Web vitals and backend timing of the page", body = PageBackendResponse),
        (status = 400, description = "A path not starting with /, or a window over 7 days"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Missing otel:read or project access"),
        (status = 500, description = "Internal server error")
    ),
    tag = "OpenTelemetry",
    security(("bearer_auth" = []))
)]
pub async fn get_page_backend(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<PageBackendParams>,
) -> Result<Json<PageBackendResponse>, Problem> {
    permission_guard!(auth, OtelRead);
    project_scope_guard!(auth, params.project_id);
    project_access_guard!(auth, params.project_id, state.project_access_checker);
    let end_time = params.end_time.unwrap_or_else(Utc::now);
    let start_time = params
        .start_time
        .unwrap_or(end_time - Duration::hours(24));
    let result = state
        .operation_trends
        .page_backend(params.project_id, &params.path, start_time, end_time)
        .await?;
    Ok(Json(page_backend_response(
        params.path,
        start_time,
        end_time,
        result,
    )))
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
    fn breakdown_shares_are_relative_to_the_operation_and_never_divide_by_zero() {
        use crate::services::operation_trends::breakdown::Breakdown;
        let params = OperationBreakdownParams {
            project_id: 1,
            service_name: "checkout-api".into(),
            span_name: "GET /cart".into(),
            start_time: None,
            end_time: None,
            sample: None,
            attribute: Some("tier".into()),
        };
        let now = Utc::now();
        let response = breakdown_response(
            &params,
            now,
            now,
            OperationBreakdown {
                sample: BreakdownSample::Slow,
                slow_threshold_ms: Some(250.0),
                breakdown: Breakdown {
                    traces: 2,
                    instances: 2,
                    total_ms: 400.0,
                    self_ms: 40.0,
                    groups: vec![BreakdownGroup {
                        service_name: "checkout-api".into(),
                        span_name: "SELECT cart_items".into(),
                        category: "database".into(),
                        total_ms: 300.0,
                        calls: 6,
                        instances: 2,
                    }],
                    by_attribute: vec![AttributeSlice {
                        value: Some("paid".into()),
                        calls: 2,
                        total_ms: 400.0,
                        avg_ms: 200.0,
                        max_ms: 250.0,
                    }],
                },
            },
        );
        assert_eq!(response.sample, "slow");
        assert_eq!(response.self_share, 0.1);
        assert_eq!(response.groups[0].share, 0.75);
        assert_eq!(response.groups[0].calls_per_instance, 3.0);
        assert_eq!(response.groups[0].avg_ms, 50.0);
        assert_eq!(response.by_attribute[0].share, 1.0);
        assert_eq!(share(5.0, 0.0), 0.0);
    }

    #[test]
    fn a_page_explains_a_missing_backend_and_compares_backend_with_ttfb() {
        let now = Utc::now();
        let empty = page_backend_response("/pricing".into(), now, now, PageBackend::default());
        assert!(!empty.configured);
        assert!(empty.reason.as_deref().is_some_and(|r| r.contains("/pricing") && r.contains("url.path")));
        assert_eq!(empty.setup_path.as_deref(), Some("traces"));
        assert_eq!(empty.backend_share_of_ttfb, None);

        let found = page_backend_response(
            "/pricing".into(),
            now,
            now,
            PageBackend {
                vitals: PageVitals {
                    page_loads: 30,
                    ttfb_p75_ms: Some(400.0),
                    ..Default::default()
                },
                requests: 12,
                p75_ms: Some(300.0),
                ..Default::default()
            },
        );
        assert!(found.configured);
        assert_eq!(found.backend_share_of_ttfb, Some(0.75));
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
