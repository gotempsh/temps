//! Query handlers for the monitoring UI.
//!
//! These endpoints are authenticated via the standard RequireAuth flow
//! (JWT/session) since they are accessed by the Temps dashboard, not by
//! OTel collectors.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

use crate::types::*;
use crate::OtelAppState;
use temps_auth::{permission_guard, project_scope_guard, RequireAuth};
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
    /// Restrict to a single metric type: gauge | sum | histogram |
    /// exponential_histogram | summary. Unknown values are ignored (no filter).
    pub metric_type: Option<String>,
    /// Aggregation applied per bucket: avg (default) | sum | min | max | count |
    /// rate | p50/p95/p99 | quantile:0.95. Unknown values fall back to avg.
    pub aggregation: Option<String>,
    /// Exact-match data-point label filters as comma-separated `key=value`
    /// pairs, e.g. `http.method=GET,http.status_code=200`. Keys must match the
    /// metric-name allowlist `[a-zA-Z0-9_.:-]`.
    pub label_filters: Option<String>,
    /// Comma-separated label keys to group the series by, e.g.
    /// `http.method,http.route`. Each key must match the allowlist.
    pub group_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MetricLabelKeysParams {
    pub project_id: i32,
    pub metric_name: String,
    /// Window start (RFC 3339). Defaults to 24h before `end_time`.
    pub start_time: Option<String>,
    /// Window end (RFC 3339). Defaults to now.
    pub end_time: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MetricLabelValuesParams {
    pub project_id: i32,
    pub metric_name: String,
    pub label_key: String,
    /// Window start (RFC 3339). Defaults to 24h before `end_time`.
    pub start_time: Option<String>,
    /// Window end (RFC 3339). Defaults to now.
    pub end_time: Option<String>,
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
    /// Filter by span attributes as comma-separated key=value pairs.
    /// e.g. "gen_ai.system=openai,gen_ai.request.model=gpt-4"
    pub attributes: Option<String>,
    /// Filter by span name pattern (ILIKE).
    pub name_pattern: Option<String>,
    /// Sort field for the trace-summaries list: "start_time" (default) or
    /// "duration". Anything else falls back to start_time.
    pub sort_by: Option<String>,
    /// Sort direction: "asc" or "desc" (default).
    pub sort_order: Option<String>,
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
pub struct OtelMetricsResponse {
    pub data: Vec<MetricBucket>,
    pub count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OtelMetricNamesResponse {
    pub names: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OtelMetricLabelKeysResponse {
    pub keys: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OtelMetricLabelValuesResponse {
    pub values: Vec<String>,
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

// ── GenAI-specific DTOs ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GenAiQueryParams {
    pub project_id: i32,
    pub service_name: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    /// Filter by gen_ai.system (e.g. "openai", "anthropic").
    pub gen_ai_system: Option<String>,
    /// Filter by gen_ai.request.model (e.g. "gpt-4", "claude-sonnet-4-20250514").
    pub gen_ai_model: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GenAiTraceSummariesResponse {
    pub data: Vec<GenAiTraceSummary>,
    pub total: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GenAiTraceDetailResponse {
    pub trace_id: String,
    pub spans: Vec<GenAiSpanDetail>,
    pub span_count: usize,
    pub events: Vec<GenAiEvent>,
    pub event_count: usize,
}

// ── Handlers ────────────────────────────────────────────────────────

fn parse_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Resolve an optional RFC-3339 (start, end) pair into a concrete window for the
/// label-discovery queries. Missing `end` → now; missing `start` → 24h before
/// `end`. Keeping the window bounded is what keeps the sampled scans cheap.
fn discovery_window(
    start: Option<&str>,
    end: Option<&str>,
) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let end = end
        .and_then(parse_datetime)
        .unwrap_or_else(chrono::Utc::now);
    let start = start
        .and_then(parse_datetime)
        .unwrap_or_else(|| end - chrono::Duration::hours(24));
    (start, end)
}

fn parse_attributes(s: &str) -> BTreeMap<String, String> {
    s.split(',')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?.trim();
            let value = parts.next()?.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Parse a metric type query token into the typed enum. Unknown → `None`.
fn parse_metric_type(s: &str) -> Option<MetricType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "gauge" => Some(MetricType::Gauge),
        "sum" | "counter" => Some(MetricType::Sum),
        "histogram" => Some(MetricType::Histogram),
        "exponential_histogram" | "exp_histogram" => Some(MetricType::ExponentialHistogram),
        "summary" => Some(MetricType::Summary),
        _ => None,
    }
}

/// Parse a comma-separated list of label keys, trimming and dropping empties.
fn parse_label_keys(s: &str) -> Vec<String> {
    s.split(',')
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .map(|k| k.to_string())
        .collect()
}

/// Parse comma-separated `key=value` label filters into ordered pairs.
fn parse_label_filters(s: &str) -> Vec<(String, String)> {
    parse_attributes(s).into_iter().collect()
}

/// Query metrics with time bucketing.
#[utoipa::path(
    tag = "Telemetry Metrics",
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
        ("metric_type" = Option<String>, Query, description = "Filter by metric type (gauge, sum, histogram, exponential_histogram, summary)"),
        ("aggregation" = Option<String>, Query, description = "Per-bucket aggregation: avg (default), sum, min, max, count, rate, p50/p95/p99, quantile:0.95"),
        ("label_filters" = Option<String>, Query, description = "Comma-separated key=value data-point label filters (keys must match [a-zA-Z0-9_.:-])"),
        ("group_by" = Option<String>, Query, description = "Comma-separated label keys to group series by"),
    ),
    responses(
        (status = 200, description = "Metrics data", body = OtelMetricsResponse),
        (status = 400, description = "Invalid label key", body = ProblemDetails),
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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, params.project_id);

    let query = MetricQuery {
        project_id: params.project_id,
        metric_name: params.metric_name,
        service_name: params.service_name,
        environment: params.environment,
        start_time: params.start_time.as_deref().and_then(parse_datetime),
        end_time: params.end_time.as_deref().and_then(parse_datetime),
        bucket_interval: params.bucket_interval,
        limit: params.limit,
        metric_type: params.metric_type.as_deref().and_then(parse_metric_type),
        label_filters: params
            .label_filters
            .as_deref()
            .map(parse_label_filters)
            .unwrap_or_default(),
        group_by: params
            .group_by
            .as_deref()
            .map(parse_label_keys)
            .unwrap_or_default(),
        aggregation: params
            .aggregation
            .as_deref()
            .map(MetricAggregation::parse)
            .unwrap_or_default(),
    };

    let data = state.otel_service.query_metrics(query).await?;
    let count = data.len();

    Ok(Json(OtelMetricsResponse { data, count }))
}

/// List distinct metric names for a project.
#[utoipa::path(
    tag = "Telemetry Metrics",
    get,
    path = "/otel/metric-names/{project_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "List of metric names", body = OtelMetricNamesResponse),
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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, project_id);

    let names = state.otel_service.list_metric_names(project_id).await?;
    Ok(Json(OtelMetricNamesResponse { names }))
}

/// List the attribute (label) keys observed on a metric — powers the
/// label-filter key autocomplete.
#[utoipa::path(
    tag = "Telemetry Metrics",
    get,
    path = "/otel/metric-label-keys",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("metric_name" = String, Query, description = "Metric to inspect"),
        ("start_time" = Option<String>, Query, description = "Window start (RFC 3339); defaults to 24h before end"),
        ("end_time" = Option<String>, Query, description = "Window end (RFC 3339); defaults to now"),
    ),
    responses(
        (status = 200, description = "Distinct label keys", body = OtelMetricLabelKeysResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_metric_label_keys(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<MetricLabelKeysParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    // Confine a project-scoped deployment token to its own project's telemetry
    // (no-op for user/API-key/session auth).
    project_scope_guard!(auth, params.project_id);

    let (start, end) = discovery_window(params.start_time.as_deref(), params.end_time.as_deref());
    let keys = state
        .otel_service
        .list_metric_label_keys(params.project_id, &params.metric_name, start, end)
        .await?;
    Ok(Json(OtelMetricLabelKeysResponse { keys }))
}

/// List the distinct values seen for a label key on a metric — powers value
/// autocomplete once a key is chosen.
#[utoipa::path(
    tag = "Telemetry Metrics",
    get,
    path = "/otel/metric-label-values",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("metric_name" = String, Query, description = "Metric to inspect"),
        ("label_key" = String, Query, description = "Label key whose values to list (must match [a-zA-Z0-9_.:-])"),
        ("start_time" = Option<String>, Query, description = "Window start (RFC 3339); defaults to 24h before end"),
        ("end_time" = Option<String>, Query, description = "Window end (RFC 3339); defaults to now"),
    ),
    responses(
        (status = 200, description = "Distinct label values", body = OtelMetricLabelValuesResponse),
        (status = 400, description = "Invalid label key", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_metric_label_values(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<MetricLabelValuesParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    // Confine a project-scoped deployment token to its own project's telemetry
    // (no-op for user/API-key/session auth).
    project_scope_guard!(auth, params.project_id);

    let (start, end) = discovery_window(params.start_time.as_deref(), params.end_time.as_deref());
    let values = state
        .otel_service
        .list_metric_label_values(
            params.project_id,
            &params.metric_name,
            &params.label_key,
            start,
            end,
        )
        .await?;
    Ok(Json(OtelMetricLabelValuesResponse { values }))
}

/// Query trace spans with optional filters.
#[utoipa::path(
    tag = "Traces",
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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, params.project_id);

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
        attributes: params
            .attributes
            .as_deref()
            .map(parse_attributes)
            .filter(|m| !m.is_empty()),
        name_pattern: params.name_pattern.clone(),
        // Sorting only applies to the trace-summaries list, not raw span queries.
        sort_by: TraceSortField::default(),
        sort_order: SortOrder::default(),
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
    tag = "Traces",
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
        ("sort_by" = Option<String>, Query, description = "Sort field: 'start_time' (default) or 'duration'"),
        ("sort_order" = Option<String>, Query, description = "Sort direction: 'asc' or 'desc' (default)"),
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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, params.project_id);

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
        attributes: params
            .attributes
            .as_deref()
            .map(parse_attributes)
            .filter(|m| !m.is_empty()),
        name_pattern: params.name_pattern.clone(),
        sort_by: params
            .sort_by
            .as_deref()
            .map(TraceSortField::parse)
            .unwrap_or_default(),
        sort_order: params
            .sort_order
            .as_deref()
            .map(SortOrder::parse)
            .unwrap_or_default(),
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
    tag = "Traces",
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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, project_id);

    let data = state.otel_service.get_trace(project_id, &trace_id).await?;
    let count = data.len();

    Ok(Json(TracesResponse { data, count }))
}

/// Query log records with optional filters.
#[utoipa::path(
    tag = "Telemetry Logs",
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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, params.project_id);

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
    tag = "Insights",
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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, project_id);

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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, project_id);

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
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, project_id);

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

// ── GenAI Agent Activity Handlers ──────────────────────────────────

/// Query GenAI trace summaries — traces containing spans with `gen_ai.*` attributes.
#[utoipa::path(
    tag = "GenAI",
    get,
    path = "/otel/genai/traces",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("service_name" = Option<String>, Query, description = "Filter by service name"),
        ("gen_ai_system" = Option<String>, Query, description = "Filter by AI system (openai, anthropic, etc.)"),
        ("gen_ai_model" = Option<String>, Query, description = "Filter by model (gpt-4, claude-sonnet-4-20250514, etc.)"),
        ("start_time" = Option<String>, Query, description = "Start time (RFC 3339)"),
        ("end_time" = Option<String>, Query, description = "End time (RFC 3339)"),
        ("limit" = Option<u64>, Query, description = "Max traces to return (default: 50, max: 100)"),
        ("offset" = Option<u64>, Query, description = "Offset for pagination"),
    ),
    responses(
        (status = 200, description = "GenAI trace summaries", body = GenAiTraceSummariesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn query_genai_traces(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<GenAiQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, params.project_id);

    // Build attribute filters. For gen_ai_system, we use gen_ai.provider.name (current)
    // but the SQL also handles the deprecated gen_ai.system via COALESCE.
    // For direct attribute filtering, we match on gen_ai.provider.name since the
    // base WHERE clause already matches spans with either attribute.
    let mut attrs = BTreeMap::new();
    if let Some(ref system) = params.gen_ai_system {
        attrs.insert("gen_ai.system".to_string(), system.clone());
    }
    if let Some(ref model) = params.gen_ai_model {
        attrs.insert("gen_ai.request.model".to_string(), model.clone());
    }

    let query = TraceQuery {
        project_id: params.project_id,
        service_name: params.service_name,
        start_time: params.start_time.as_deref().and_then(parse_datetime),
        end_time: params.end_time.as_deref().and_then(parse_datetime),
        attributes: if attrs.is_empty() {
            None
        } else {
            Some(attrs.clone())
        },
        limit: params.limit,
        offset: params.offset,
        ..Default::default()
    };

    let count_query = TraceQuery {
        limit: None,
        offset: None,
        ..query.clone()
    };

    let data = state
        .otel_service
        .query_genai_trace_summaries(query)
        .await?;
    let total = state.otel_service.count_genai_traces(count_query).await?;

    Ok(Json(GenAiTraceSummariesResponse { data, total }))
}

/// Get GenAI span details for a specific trace.
#[utoipa::path(
    tag = "GenAI",
    get,
    path = "/otel/genai/traces/{project_id}/{trace_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("trace_id" = String, Path, description = "Trace ID (hex)"),
    ),
    responses(
        (status = 200, description = "GenAI trace span details", body = GenAiTraceDetailResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_genai_trace(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path((project_id, trace_id)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, project_id);

    let spans = state
        .otel_service
        .get_genai_trace_spans(project_id, &trace_id)
        .await?;
    let span_count = spans.len();

    let events = state
        .otel_service
        .get_genai_trace_events(project_id, &trace_id)
        .await?;
    let event_count = events.len();

    Ok(Json(GenAiTraceDetailResponse {
        trace_id,
        spans,
        span_count,
        events,
        event_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_attributes_single_pair() {
        let result = parse_attributes("gen_ai.system=openai");
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("gen_ai.system").unwrap(), "openai");
    }

    #[test]
    fn test_parse_attributes_multiple_pairs() {
        let result = parse_attributes("gen_ai.system=openai,gen_ai.request.model=gpt-4");
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("gen_ai.system").unwrap(), "openai");
        assert_eq!(result.get("gen_ai.request.model").unwrap(), "gpt-4");
    }

    #[test]
    fn test_parse_attributes_with_whitespace() {
        let result = parse_attributes(" gen_ai.system = openai , gen_ai.request.model = gpt-4 ");
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("gen_ai.system").unwrap(), "openai");
    }

    #[test]
    fn test_parse_attributes_empty_string() {
        let result = parse_attributes("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_attributes_value_with_equals() {
        let result = parse_attributes("key=value=with=equals");
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("key").unwrap(), "value=with=equals");
    }

    #[test]
    fn test_parse_attributes_skips_invalid_pairs() {
        let result = parse_attributes("valid=ok,,novalue,=emptykey,good=yes");
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("valid").unwrap(), "ok");
        assert_eq!(result.get("good").unwrap(), "yes");
    }

    // ── Metric query param parsing ──────────────────────────────────

    #[test]
    fn test_parse_metric_type() {
        assert_eq!(parse_metric_type("gauge"), Some(MetricType::Gauge));
        assert_eq!(parse_metric_type("Sum"), Some(MetricType::Sum));
        assert_eq!(parse_metric_type("counter"), Some(MetricType::Sum));
        assert_eq!(parse_metric_type("histogram"), Some(MetricType::Histogram));
        assert_eq!(
            parse_metric_type("exponential_histogram"),
            Some(MetricType::ExponentialHistogram)
        );
        assert_eq!(parse_metric_type("summary"), Some(MetricType::Summary));
        assert_eq!(parse_metric_type("nonsense"), None);
    }

    #[test]
    fn test_parse_label_keys() {
        let keys = parse_label_keys("http.method, http.route ,,");
        assert_eq!(
            keys,
            vec!["http.method".to_string(), "http.route".to_string()]
        );
        assert!(parse_label_keys("").is_empty());
        assert!(parse_label_keys("  ,  , ").is_empty());
    }

    #[test]
    fn test_parse_label_filters() {
        let filters = parse_label_filters("http.method=GET,http.status_code=200");
        // parse_attributes returns a BTreeMap (sorted), so order is deterministic.
        assert_eq!(
            filters,
            vec![
                ("http.method".to_string(), "GET".to_string()),
                ("http.status_code".to_string(), "200".to_string()),
            ]
        );
        assert!(parse_label_filters("").is_empty());
    }

    #[test]
    fn test_discovery_window_defaults_to_last_24h() {
        // No bounds → [now-24h, now], so the span is ~24h.
        let (start, end) = discovery_window(None, None);
        let span = end - start;
        assert_eq!(span.num_hours(), 24);
    }

    #[test]
    fn test_discovery_window_start_defaults_relative_to_end() {
        // Explicit end, missing start → start is 24h before that end (not now).
        let (start, end) = discovery_window(None, Some("2026-01-10T12:00:00Z"));
        assert_eq!(end.to_rfc3339(), "2026-01-10T12:00:00+00:00");
        assert_eq!(start.to_rfc3339(), "2026-01-09T12:00:00+00:00");
    }

    #[test]
    fn test_discovery_window_honors_both_bounds() {
        let (start, end) =
            discovery_window(Some("2026-01-01T00:00:00Z"), Some("2026-01-02T00:00:00Z"));
        assert_eq!(start.to_rfc3339(), "2026-01-01T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-01-02T00:00:00+00:00");
    }
}
