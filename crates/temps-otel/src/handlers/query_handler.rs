//! Query handlers for the monitoring UI.
//!
//! These endpoints are authenticated via the standard RequireAuth flow
//! (JWT/session) since they are accessed by the Temps dashboard, not by
//! OTel collectors.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::types::*;
use crate::OtelAppState;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::ProblemDetails;

// ── Request DTOs ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MetricQueryParams {
    pub project_id: i32,
    pub metric_name: Option<String>,
    pub service_name: Option<String>,
    pub environment: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub bucket_interval: Option<String>,
    pub limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct TraceQueryParams {
    pub project_id: i32,
    pub trace_id: Option<String>,
    pub service_name: Option<String>,
    pub status: Option<String>,
    pub min_duration_ms: Option<f64>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct LogQueryParams {
    pub project_id: i32,
    pub severity: Option<String>,
    pub service_name: Option<String>,
    pub search: Option<String>,
    pub trace_id: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct InsightQueryParams {
    pub status: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct HealthQueryParams {
    pub environment_id: Option<i32>,
}

// ── Response DTOs ───────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct MetricsResponse {
    pub data: Vec<MetricBucket>,
    pub count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MetricNamesResponse {
    pub names: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TracesResponse {
    pub data: Vec<SpanRecord>,
    pub count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TraceSummariesResponse {
    pub data: Vec<TraceSummary>,
    pub total: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LogsResponse {
    pub data: Vec<LogRecord>,
    pub count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InsightsResponse {
    pub data: Vec<Insight>,
    pub count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    pub summaries: Vec<HealthSummary>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QuotaResponse {
    pub quota: StorageQuota,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PipelineStatsResponse {
    pub stats: PipelineStats,
}

// ── Handlers ────────────────────────────────────────────────────────

fn parse_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Query metrics with time bucketing.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/metrics",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("metric_name" = Option<String>, Query, description = "Filter by metric name"),
        ("service_name" = Option<String>, Query, description = "Filter by service name"),
        ("environment" = Option<String>, Query, description = "Filter by deployment environment"),
        ("start_time" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("end_time" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("bucket_interval" = Option<String>, Query, description = "Bucket interval (e.g. '1 hour', '5 minutes')"),
        ("limit" = Option<u64>, Query, description = "Max buckets to return (default: 1000)"),
    ),
    responses(
        (status = 200, description = "Metrics data", body = MetricsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn query_metrics(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<MetricQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let query = MetricQuery {
        project_id: params.project_id,
        metric_name: params.metric_name,
        service_name: params.service_name,
        environment: params.environment,
        start_time: params.start_time.as_deref().and_then(parse_datetime),
        end_time: params.end_time.as_deref().and_then(parse_datetime),
        bucket_interval: params.bucket_interval,
        limit: params.limit,
    };

    let data = state.otel_service.query_metrics(query).await?;
    let count = data.len();

    Ok(Json(MetricsResponse { data, count }))
}

/// List distinct metric names for a project.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/metric-names/{project_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "List of metric names", body = MetricNamesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_metric_names(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let names = state.otel_service.list_metric_names(project_id).await?;
    Ok(Json(MetricNamesResponse { names }))
}

/// Query trace spans with optional filters.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/traces",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("trace_id" = Option<String>, Query, description = "Filter by trace ID"),
        ("service_name" = Option<String>, Query, description = "Filter by service name"),
        ("status" = Option<String>, Query, description = "Filter by status (OK, ERROR, UNSET)"),
        ("min_duration_ms" = Option<f64>, Query, description = "Minimum span duration in ms"),
        ("start_time" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("end_time" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("deployment_id" = Option<i32>, Query, description = "Filter by deployment ID"),
        ("limit" = Option<u64>, Query, description = "Max spans to return (default: 100, max: 1000)"),
        ("offset" = Option<u64>, Query, description = "Offset for pagination"),
    ),
    responses(
        (status = 200, description = "Trace spans", body = TracesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn query_traces(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<TraceQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let status = params.status.as_deref().map(|s| match s {
        "OK" | "ok" => SpanStatusCode::Ok,
        "ERROR" | "error" => SpanStatusCode::Error,
        _ => SpanStatusCode::Unset,
    });

    let query = TraceQuery {
        project_id: params.project_id,
        trace_id: params.trace_id,
        service_name: params.service_name,
        status,
        min_duration_ms: params.min_duration_ms,
        start_time: params.start_time.as_deref().and_then(parse_datetime),
        end_time: params.end_time.as_deref().and_then(parse_datetime),
        environment_id: params.environment_id,
        deployment_id: params.deployment_id,
        limit: params.limit,
        offset: params.offset,
    };

    let data = state.otel_service.query_spans(query).await?;
    let count = data.len();

    Ok(Json(TracesResponse { data, count }))
}

/// Query trace summaries — one row per trace with span count, error count,
/// root span info, and proper trace-level pagination.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/trace-summaries",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("trace_id" = Option<String>, Query, description = "Filter by trace ID"),
        ("service_name" = Option<String>, Query, description = "Filter by service name"),
        ("status" = Option<String>, Query, description = "Filter by status (OK, ERROR)"),
        ("min_duration_ms" = Option<f64>, Query, description = "Minimum trace duration in ms"),
        ("start_time" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("end_time" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("deployment_id" = Option<i32>, Query, description = "Filter by deployment ID"),
        ("limit" = Option<u64>, Query, description = "Max traces to return (default: 50, max: 100)"),
        ("offset" = Option<u64>, Query, description = "Offset for pagination"),
    ),
    responses(
        (status = 200, description = "Trace summaries", body = TraceSummariesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn query_trace_summaries(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<TraceQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let status = params.status.as_deref().map(|s| match s {
        "OK" | "ok" => SpanStatusCode::Ok,
        "ERROR" | "error" => SpanStatusCode::Error,
        _ => SpanStatusCode::Unset,
    });

    let query = TraceQuery {
        project_id: params.project_id,
        trace_id: params.trace_id,
        service_name: params.service_name,
        status,
        min_duration_ms: params.min_duration_ms,
        start_time: params.start_time.as_deref().and_then(parse_datetime),
        end_time: params.end_time.as_deref().and_then(parse_datetime),
        environment_id: params.environment_id,
        deployment_id: params.deployment_id,
        limit: params.limit,
        offset: params.offset,
    };

    // Clone query for the count call (which ignores limit/offset)
    let count_query = TraceQuery {
        limit: None,
        offset: None,
        ..query.clone()
    };

    let data = state.otel_service.query_trace_summaries(query).await?;
    let total = state.otel_service.count_traces(count_query).await?;

    Ok(Json(TraceSummariesResponse { data, total }))
}

/// Get all spans for a specific trace.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/traces/{project_id}/{trace_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("trace_id" = String, Path, description = "Trace ID (hex)"),
    ),
    responses(
        (status = 200, description = "Trace spans tree", body = TracesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_trace(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path((project_id, trace_id)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let data = state.otel_service.get_trace(project_id, &trace_id).await?;
    let count = data.len();

    Ok(Json(TracesResponse { data, count }))
}

/// Query log records with optional filters.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/logs",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("severity" = Option<String>, Query, description = "Filter by severity (TRACE, DEBUG, INFO, WARN, ERROR, FATAL)"),
        ("service_name" = Option<String>, Query, description = "Filter by service name"),
        ("search" = Option<String>, Query, description = "Full-text search in log body (ILIKE)"),
        ("trace_id" = Option<String>, Query, description = "Filter by correlated trace ID"),
        ("start_time" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("end_time" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("limit" = Option<u64>, Query, description = "Max logs to return (default: 100, max: 1000)"),
        ("offset" = Option<u64>, Query, description = "Offset for pagination"),
    ),
    responses(
        (status = 200, description = "Log records", body = LogsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn query_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<LogQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let severity = params.severity.as_deref().map(|s| match s {
        "TRACE" | "trace" => LogSeverity::Trace,
        "DEBUG" | "debug" => LogSeverity::Debug,
        "INFO" | "info" => LogSeverity::Info,
        "WARN" | "warn" => LogSeverity::Warn,
        "ERROR" | "error" => LogSeverity::Error,
        "FATAL" | "fatal" => LogSeverity::Fatal,
        _ => LogSeverity::Info,
    });

    let query = LogQuery {
        project_id: params.project_id,
        severity,
        service_name: params.service_name,
        search: params.search,
        trace_id: params.trace_id,
        start_time: params.start_time.as_deref().and_then(parse_datetime),
        end_time: params.end_time.as_deref().and_then(parse_datetime),
        limit: params.limit,
        offset: params.offset,
    };

    let data = state.otel_service.query_logs(query).await?;
    let count = data.len();

    Ok(Json(LogsResponse { data, count }))
}

/// List anomaly insights for a project.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/insights/{project_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("status" = Option<String>, Query, description = "Filter by status (active, resolved)"),
        ("limit" = Option<u64>, Query, description = "Max insights to return (default: 20, max: 100)"),
        ("offset" = Option<u64>, Query, description = "Offset for pagination"),
    ),
    responses(
        (status = 200, description = "Insights list", body = InsightsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_insights(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(project_id): Path<i32>,
    Query(params): Query<InsightQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let status = params.status.as_deref().map(|s| match s {
        "resolved" => InsightStatus::Resolved,
        _ => InsightStatus::Active,
    });

    let limit = params.limit.unwrap_or(20).min(100);
    let offset = params.offset.unwrap_or(0);

    let data = state
        .otel_service
        .list_insights(project_id, status, limit, offset)
        .await?;
    let count = data.len();

    Ok(Json(InsightsResponse { data, count }))
}

/// Get health summaries for a project.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/health/{project_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
    ),
    responses(
        (status = 200, description = "Health summaries", body = HealthResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_health(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(project_id): Path<i32>,
    Query(params): Query<HealthQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let summaries = state
        .otel_service
        .get_health_summaries(project_id, params.environment_id)
        .await?;

    Ok(Json(HealthResponse { summaries }))
}

/// Get storage quota for a project.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/quota/{project_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Storage quota", body = QuotaResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_quota(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let quota = state.otel_service.get_storage_quota(project_id).await?;
    Ok(Json(QuotaResponse { quota }))
}

/// Get OTel pipeline statistics (admin/system view).
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/pipeline-stats",
    responses(
        (status = 200, description = "Pipeline statistics", body = PipelineStatsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_pipeline_stats(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let stats = state.otel_service.pipeline_stats();
    Ok(Json(PipelineStatsResponse { stats }))
}
