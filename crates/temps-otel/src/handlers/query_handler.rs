//! Query handlers for the monitoring UI.
//!
//! These endpoints are authenticated via the standard RequireAuth flow
//! (JWT/session) since they are accessed by the Temps dashboard, not by
//! OTel collectors.

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::warn;
use utoipa::ToSchema;

use crate::handlers::audit::{CrossProjectTraceSiblingsReadAudit, UnifiedTraceReadAudit};
use crate::services::cross_project::is_valid_trace_id;
use crate::services::{CrossProjectTraceError, SiblingRef, UnifiedTrace};
use crate::types::*;
use crate::OtelAppState;
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, ProblemDetails, RequestMetadata};

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
    /// Whether to compute `total` on the trace-summaries list. Defaults to
    /// true. Set false when the caller only needs the page itself (an
    /// existence probe, a poll, an infinite-scroll feed): the total is a
    /// second aggregation over the whole window, and skipping it removes one
    /// of the two queries the endpoint would otherwise issue.
    pub include_total: Option<bool>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Query parameters for `GET /otel/span-stats`.
///
/// Every filter is optional except the project selection: pass `project_id` for
/// one project, or `project_ids` (comma-separated) to rank operations across
/// several at once. Each project is access-checked individually.
#[derive(Debug, Deserialize)]
pub struct SpanStatsQueryParams {
    /// Single project to report on. Ignored when `project_ids` is given.
    pub project_id: Option<i32>,
    /// Comma-separated project ids, e.g. `4,5,6`. At most
    /// [`SPAN_STATS_MAX_PROJECTS`].
    pub project_ids: Option<String>,
    /// Window start (RFC 3339). Defaults to 24h before `end_time`.
    pub start_time: Option<String>,
    /// Window end (RFC 3339). Defaults to now.
    pub end_time: Option<String>,
    pub service_name: Option<String>,
    /// Exact span name — "how slow did *this* operation get?".
    pub span_name: Option<String>,
    /// Substring match on the span name (case-insensitive).
    pub name_pattern: Option<String>,
    /// `server` | `client` | `internal` | `producer` | `consumer`.
    pub kind: Option<String>,
    /// `ok` | `error` | `unset`. `error` answers "how slow are the failures?".
    pub status: Option<String>,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    /// Span attribute filters as comma-separated `key=value` pairs.
    pub attributes: Option<String>,
    /// Ignore spans faster than this before aggregating.
    pub min_duration_ms: Option<f64>,
    /// Drop operations with fewer than this many samples (default 1).
    pub min_count: Option<u64>,
    /// `total_time` (default) | `p50` | `p95` | `p99` | `max` | `avg` |
    /// `stddev` | `count` | `errors` | `error_rate` | `variability` | `tail_ratio`.
    pub sort_by: Option<String>,
    /// `asc` | `desc` (default).
    pub sort_order: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Response for `GET /otel/span-stats`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SpanStatsResponse {
    pub data: Vec<SpanStats>,
    /// Total number of distinct operations matching the filters, for pagination.
    pub total: u64,
    /// The window actually aggregated, echoed back because it is defaulted
    /// server-side when the caller omits it.
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
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
    /// Total traces matching the filters, ignoring pagination. Omitted when
    /// the request passed `include_total=false`, in which case the caller
    /// asked not to pay for the count — treat its absence as "unknown", not
    /// as zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
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
pub struct HasTracesResponse {
    pub has_traces: bool,
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
    project_access_guard!(auth, params.project_id, state.project_access_checker);

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
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_access_guard!(auth, params.project_id, state.project_access_checker);

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
    project_access_guard!(auth, params.project_id, state.project_access_checker);

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
///
/// Each returned span has a `duration_ms` field (float, milliseconds) — this is
/// the ONLY field guaranteed to be in milliseconds. Spans also carry an
/// `attributes` map of raw key/value pairs exactly as reported by the
/// instrumenting library: numeric attribute values may be seconds, milliseconds,
/// microseconds, or nanoseconds depending on that library's convention, and
/// nothing in this response labels the unit. Never assume an attribute's
/// numeric value shares `duration_ms`'s unit, and never state a duration in
/// milliseconds unless it came from a `duration_ms` field.
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
        ("attributes" = Option<String>, Query, description = "Filter by span attributes as comma-separated key=value pairs, e.g. \"gen_ai.system=openai,gen_ai.request.model=gpt-4\""),
        ("name_pattern" = Option<String>, Query, description = "Filter by span name pattern (ILIKE)"),
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
    project_access_guard!(auth, params.project_id, state.project_access_checker);

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
        root_only: false,
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
        ("name_pattern" = Option<String>, Query, description = "Filter by span name pattern (ILIKE)"),
        ("sort_by" = Option<String>, Query, description = "Sort field: 'start_time' (default) or 'duration'"),
        ("sort_order" = Option<String>, Query, description = "Sort direction: 'asc' or 'desc' (default)"),
        ("include_total" = Option<bool>, Query, description = "Compute the `total` count (default: true). Set false to skip the second aggregation when only the page is needed"),
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
    project_access_guard!(auth, params.project_id, state.project_access_checker);

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
        root_only: false,
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

    // The page and the total are independent aggregations over the same
    // window, so issue them concurrently rather than paying for both in
    // series. `include_total=false` skips the second one entirely.
    let (mut data, total) = if params.include_total.unwrap_or(true) {
        // Clone query for the count call (which ignores limit/offset)
        let count_query = TraceQuery {
            limit: None,
            offset: None,
            ..query.clone()
        };
        let (data, total) = tokio::try_join!(
            state.otel_service.query_trace_summaries(query),
            state.otel_service.count_traces(count_query),
        )?;
        (data, Some(total))
    } else {
        (state.otel_service.query_trace_summaries(query).await?, None)
    };

    // Name cross-project trace rows whose root span lives in a sibling project:
    // when this project holds only child spans, the summary has no root and would
    // render as "(unnamed)". Backfill the root name/service from the root-owning
    // (sharing) project. Best-effort — a failure leaves the rows as-is.
    let unnamed: Vec<String> = data
        .iter()
        .filter(|s| s.root_span_name.trim().is_empty())
        .map(|s| s.trace_id.clone())
        .collect();
    if !unnamed.is_empty() {
        match state
            .cross_project_service
            .resolve_root_names(&unnamed)
            .await
        {
            Ok(names) => {
                for s in data.iter_mut() {
                    if let Some((name, svc)) = names.get(&s.trace_id) {
                        if s.root_span_name.trim().is_empty() {
                            s.root_span_name = name.clone();
                        }
                        if s.service_name.trim().is_empty() {
                            s.service_name = svc.clone();
                        }
                    }
                }
            }
            Err(e) => warn!("cross-project trace name backfill failed: {e}"),
        }
    }

    Ok(Json(TraceSummariesResponse { data, total }))
}

/// Rank operations by latency, volume, or inconsistency.
///
/// Groups spans by `(project, service, span name)` over a bounded window and
/// returns count, error rate, total/min/max/avg/stddev duration, p50/p95/p99,
/// and two variability ratios per operation. Sorting is what makes it useful:
///
/// - `sort_by=total_time` (default) — where the wall-clock actually goes.
/// - `sort_by=p95` / `p99` — what users actually feel.
/// - `sort_by=variability` or `tail_ratio` — operations whose *spread* is the
///   problem: the ones that take 40ms most of the time and 4s the rest.
/// - `span_name=payments.charge` — the worst this one operation ever got, in
///   `max_duration_ms`.
///
/// Pair the variability sorts with `min_count` — a ratio computed from three
/// samples is noise, and without a floor it outranks every real signal.
///
/// Two bounds are enforced rather than clamped, so a result never claims to
/// cover more than it does: at most 50 projects, and a window no wider than
/// 31 days. Both return 400. Unlike the trace list this report has no early
/// exit — it aggregates every span in the window before it can rank anything.
#[utoipa::path(
    tag = "Traces",
    get,
    path = "/otel/span-stats",
    params(
        ("project_id" = Option<i32>, Query, description = "Single project to report on"),
        ("project_ids" = Option<String>, Query, description = "Comma-separated project ids, e.g. `4,5,6` (max 50)"),
        ("start_time" = Option<String>, Query, description = "Window start (RFC 3339); defaults to 24h before end_time. The window may not exceed 31 days"),
        ("end_time" = Option<String>, Query, description = "Window end (RFC 3339); defaults to now"),
        ("service_name" = Option<String>, Query, description = "Restrict to one service"),
        ("span_name" = Option<String>, Query, description = "Restrict to one operation by exact span name"),
        ("name_pattern" = Option<String>, Query, description = "Case-insensitive substring match on the span name"),
        ("kind" = Option<String>, Query, description = "server | client | internal | producer | consumer"),
        ("status" = Option<String>, Query, description = "ok | error | unset"),
        ("environment_id" = Option<i32>, Query, description = "Restrict to one environment"),
        ("deployment_id" = Option<i32>, Query, description = "Restrict to one deployment"),
        ("attributes" = Option<String>, Query, description = "Comma-separated key=value span attribute filters"),
        ("min_duration_ms" = Option<f64>, Query, description = "Ignore spans faster than this"),
        ("min_count" = Option<u64>, Query, description = "Drop operations with fewer samples than this"),
        ("sort_by" = Option<String>, Query, description = "total_time | p50 | p95 | p99 | max | avg | stddev | count | errors | error_rate | variability | tail_ratio"),
        ("sort_order" = Option<String>, Query, description = "asc | desc (default)"),
        ("limit" = Option<u64>, Query, description = "Page size (default 20, max 100)"),
        ("offset" = Option<u64>, Query, description = "Page offset"),
    ),
    responses(
        (status = 200, description = "Per-operation latency statistics", body = SpanStatsResponse),
        (status = 400, description = "Invalid query (no project, empty window)", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn query_span_stats(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<SpanStatsQueryParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let project_ids = parse_project_ids(&params)?;
    // Authorize every project individually. A multi-project report must not
    // become a way to read a project the caller cannot open on its own.
    for project_id in &project_ids {
        project_scope_guard!(auth, *project_id);
        project_access_guard!(auth, *project_id, state.project_access_checker);
    }

    let (start_time, end_time) =
        discovery_window(params.start_time.as_deref(), params.end_time.as_deref());

    let query = SpanStatsQuery {
        project_ids,
        start_time,
        end_time,
        service_name: params.service_name.clone(),
        span_name: params.span_name.clone(),
        name_pattern: params.name_pattern.clone().filter(|p| !p.is_empty()),
        kind: params.kind.as_deref().and_then(parse_span_kind_param),
        status: params.status.as_deref().and_then(parse_span_status_param),
        environment_id: params.environment_id,
        deployment_id: params.deployment_id,
        attributes: params
            .attributes
            .as_deref()
            .map(parse_attributes)
            .filter(|m| !m.is_empty()),
        min_duration_ms: params.min_duration_ms,
        min_count: params.min_count.unwrap_or(1).max(1),
        sort_by: params
            .sort_by
            .as_deref()
            .map(SpanStatsSortField::parse)
            .unwrap_or_default(),
        sort_order: params
            .sort_order
            .as_deref()
            .map(SortOrder::parse)
            .unwrap_or_default(),
        limit: params.limit,
        offset: params.offset,
    };

    // The count ignores limit/offset but must keep every other filter, or the
    // total disagrees with the page.
    let count_query = SpanStatsQuery {
        limit: None,
        offset: None,
        ..query.clone()
    };

    let (data, total) = tokio::try_join!(
        state.otel_service.query_span_stats(query),
        state.otel_service.count_span_stats(count_query),
    )?;

    Ok(Json(SpanStatsResponse {
        data,
        total,
        start_time,
        end_time,
    }))
}

/// Resolve the requested projects from `project_ids` (preferred) or the
/// single-project `project_id`.
///
/// Rejects an empty selection with a 400 rather than silently reporting on
/// nothing: a typo in `project_ids` would otherwise return an empty table that
/// looks exactly like "this project has no traces".
fn parse_project_ids(params: &SpanStatsQueryParams) -> Result<Vec<i32>, Problem> {
    let mut ids: Vec<i32> = match params.project_ids.as_deref() {
        Some(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<i32>().map_err(|_| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Invalid Project Ids")
                        .with_detail(format!("'{s}' in project_ids is not an integer"))
                })
            })
            .collect::<Result<Vec<i32>, Problem>>()?,
        None => params.project_id.into_iter().collect(),
    };
    ids.sort_unstable();
    ids.dedup();

    if ids.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Project Required")
            .with_detail(
                "Provide project_id, or project_ids as a comma-separated list of project ids",
            ));
    }
    // Rejected here, before the per-project access checks run, so an oversized
    // list costs one string parse rather than one authorization round-trip per
    // id. The service re-checks the same bound for non-HTTP callers.
    if ids.len() > SPAN_STATS_MAX_PROJECTS {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Too Many Projects")
            .with_detail(format!(
                "span-stats accepts at most {} projects per query, got {}",
                SPAN_STATS_MAX_PROJECTS,
                ids.len()
            )));
    }
    Ok(ids)
}

/// Parse a span-kind query token. Unknown → `None` (no filter).
fn parse_span_kind_param(s: &str) -> Option<SpanKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "server" => Some(SpanKind::Server),
        "client" => Some(SpanKind::Client),
        "internal" => Some(SpanKind::Internal),
        "producer" => Some(SpanKind::Producer),
        "consumer" => Some(SpanKind::Consumer),
        "unspecified" => Some(SpanKind::Unspecified),
        _ => None,
    }
}

/// Parse a span-status query token. Unknown → `None` (no filter).
fn parse_span_status_param(s: &str) -> Option<SpanStatusCode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ok" => Some(SpanStatusCode::Ok),
        "error" => Some(SpanStatusCode::Error),
        "unset" => Some(SpanStatusCode::Unset),
        _ => None,
    }
}

/// Get all spans for a specific trace.
///
/// Each span has a `duration_ms` field (float, milliseconds) — the ONLY field
/// guaranteed to be in milliseconds — plus an `attributes` map of raw
/// key/value pairs exactly as the instrumenting library reported them.
/// Numeric attribute values (e.g. connection-pool wait times, queue delays)
/// may be in seconds, milliseconds, microseconds, or nanoseconds depending on
/// that library's own convention; this response never labels the unit. When
/// explaining what a span spent time on, only quote milliseconds from
/// `duration_ms` (or from `start_time`/`end_time` deltas) — never assume a raw
/// attribute number is already in milliseconds.
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
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_access_guard!(auth, params.project_id, state.project_access_checker);

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
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_access_guard!(auth, project_id, state.project_access_checker);

    let quota = state.otel_service.get_storage_quota(project_id).await?;
    Ok(Json(QuotaResponse { quota }))
}

/// Whether a project has ever received at least one trace span.
///
/// A pure existence check for onboarding/setup UI (e.g. "has this project
/// set up OpenTelemetry yet?"). Deliberately not `/otel/trace-summaries`
/// with `limit=1`: that endpoint aggregates by trace (`GROUP BY trace_id`,
/// `argMax`) and, without a time bound, that aggregation runs over every
/// span the project has ever ingested. This endpoint answers the same
/// yes/no question in O(1) — see `OtelStorage::has_traces`.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/has-traces/{project_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Trace existence check", body = HasTracesResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn has_traces(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    // Confine a project-scoped deployment token to its own project (no-op for
    // user/API-key/session auth).
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let has_traces = state.otel_service.has_traces(project_id).await?;
    Ok(Json(HasTracesResponse { has_traces }))
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
///
/// `duration_ms` is the only field guaranteed to be milliseconds. `gen_ai.*`
/// span attributes (e.g. time-to-first-token, token latency) often follow the
/// OTel GenAI semantic conventions, which use **seconds** (a fractional
/// double), not milliseconds — do not read them as ms without converting.
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
    project_access_guard!(auth, params.project_id, state.project_access_checker);

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
///
/// `duration_ms` is the only field guaranteed to be milliseconds. `gen_ai.*`
/// span attributes (e.g. time-to-first-token, token latency) often follow the
/// OTel GenAI semantic conventions, which use **seconds** (a fractional
/// double), not milliseconds — do not read them as ms without converting.
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
    project_access_guard!(auth, project_id, state.project_access_checker);

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

// ── Cross-project trace endpoints (ADR-027) ─────────────────────────

/// Convert `CrossProjectTraceError` into an RFC-7807 Problem response.
///
/// `InvalidTraceId` → 400; all database/storage errors → 500.
/// The match is exhaustive with no catch-all arm per CLAUDE.md rules.
impl From<CrossProjectTraceError> for Problem {
    fn from(error: CrossProjectTraceError) -> Self {
        match error {
            CrossProjectTraceError::InvalidTraceId { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Trace ID")
                    .with_detail(error.to_string())
            }
            CrossProjectTraceError::QuerySiblings { .. }
            | CrossProjectTraceError::QueryProjects { .. }
            | CrossProjectTraceError::Database(_)
            | CrossProjectTraceError::Storage(_) => {
                // Log the real error server-side only — DB/storage error text
                // can contain schema/table names or paths that must not reach
                // the caller. Same pattern as `ingest_handler`'s
                // `From<OtelError> for Problem`.
                warn!(error = %error, "Cross-project trace query internal error");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("An internal error occurred")
            }
        }
    }
}

// ── Response DTOs ───────────────────────────────────────────────────

/// A sibling project that shares the same `trace_id`, returned by the
/// Phase 1 cross-project banner endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct CrossProjectSiblingRef {
    pub project_id: i32,
    pub project_name: String,
    /// URL slug used to link into the sibling project's single-project trace view.
    pub project_slug: String,
    /// ISO 8601 timestamp (UTC, `Z` suffix) of first span ingest for this
    /// `(trace_id, project_id)` pair.
    #[schema(value_type = String, format = DateTime)]
    pub first_seen: DateTime<Utc>,
}

impl From<SiblingRef> for CrossProjectSiblingRef {
    fn from(s: SiblingRef) -> Self {
        Self {
            project_id: s.project_id,
            project_name: s.project_name,
            project_slug: s.project_slug,
            first_seen: s.first_seen,
        }
    }
}

/// Response body for `GET /otel/traces/cross-project/{trace_id}`.
///
/// An empty `siblings` vec is the normal single-project case — never 404.
#[derive(Debug, Serialize, ToSchema)]
pub struct CrossProjectTraceResponse {
    /// The trace_id that was queried (echoed back for client convenience).
    pub trace_id: String,
    /// Projects other than the caller's that hold spans for this trace,
    /// ordered by `first_seen ASC`.
    pub siblings: Vec<CrossProjectSiblingRef>,
}

/// Query parameters for `GET /otel/traces/cross-project/{trace_id}`.
#[derive(Debug, Deserialize)]
pub struct CrossProjectSiblingsParams {
    /// The caller's own project ID — excluded from the sibling list so the
    /// UI does not render a self-link.
    pub exclude_project_id: Option<i32>,
}

// ── Handlers ────────────────────────────────────────────────────────

/// Discover sibling projects that share the same `trace_id` (Phase 1 banner).
///
/// Returns an empty `siblings` list when the trace is single-project — never
/// 404. Project names are included so the UI can render navigation links
/// without a second round-trip.  See ADR-027 §3 for the full auth model and
/// topology-disclosure trade-offs.
#[utoipa::path(
    tag = "Traces",
    get,
    path = "/otel/traces/cross-project/{trace_id}",
    operation_id = "getCrossProjectTraceSiblings",
    params(
        ("trace_id" = String, Path, description = "Trace ID (32 lowercase hex characters)"),
        ("exclude_project_id" = Option<i32>, Query,
         description = "Project ID to exclude (the caller's own project) so the UI \
                        does not render a self-link"),
    ),
    responses(
        (status = 200, description = "Sibling projects sharing this trace",
         body = CrossProjectTraceResponse),
        (status = 400, description = "trace_id is not 32 lowercase hex characters",
         body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions or deployment token",
         body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_cross_project_trace_siblings(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(trace_id): Path<String>,
    Query(params): Query<CrossProjectSiblingsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    deny_deployment_token!(auth);

    if !is_valid_trace_id(&trace_id) {
        return Err(CrossProjectTraceError::InvalidTraceId {
            trace_id: trace_id.clone(),
        }
        .into());
    }

    // Emit audit log before any database access (ADR-027 §1).
    // Failure is warn!-logged and never propagates to the caller.
    let audit = CrossProjectTraceSiblingsReadAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        trace_id: trace_id.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        warn!(
            error = %e,
            trace_id = %trace_id,
            "Failed to write cross-project trace siblings audit log (non-fatal)"
        );
    }

    let siblings = state
        .cross_project_service
        .find_sibling_projects(&trace_id, params.exclude_project_id)
        .await
        .map_err(Problem::from)?;

    let siblings: Vec<CrossProjectSiblingRef> = siblings
        .into_iter()
        .map(CrossProjectSiblingRef::from)
        .collect();

    Ok(Json(CrossProjectTraceResponse { trace_id, siblings }))
}

/// Assemble a unified cross-project span waterfall (Phase 2).
///
/// Fans out to every project that holds spans for `trace_id` (up to 20
/// projects, 10,000 total spans).  Spans are annotated with
/// `project_id`/`project_name` and sorted by `start_time ASC`.
/// `truncated: true` signals a hit on either cap; `truncated_projects`
/// lists the dropped project IDs.  See ADR-027 §4 for the full design.
#[utoipa::path(
    tag = "Traces",
    get,
    path = "/otel/global/traces/{trace_id}",
    operation_id = "getUnifiedTrace",
    params(
        ("trace_id" = String, Path, description = "Trace ID (32 lowercase hex characters)"),
    ),
    responses(
        (status = 200, description = "Unified cross-project trace waterfall",
         body = UnifiedTrace),
        (status = 400, description = "trace_id is not 32 lowercase hex characters",
         body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions or deployment token",
         body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_unified_trace(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(trace_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    deny_deployment_token!(auth);

    if !is_valid_trace_id(&trace_id) {
        return Err(CrossProjectTraceError::InvalidTraceId {
            trace_id: trace_id.clone(),
        }
        .into());
    }

    // Emit audit log before fan-out begins (ADR-027 §1).
    // Failure is warn!-logged and never propagates to the caller.
    let audit = UnifiedTraceReadAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        trace_id: trace_id.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        warn!(
            error = %e,
            trace_id = %trace_id,
            "Failed to write unified trace audit log (non-fatal)"
        );
    }

    let unified: UnifiedTrace = state
        .cross_project_service
        .get_unified_trace(&trace_id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(unified))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `include_total=false` must OMIT the key rather than report 0. A client
    /// that reads `total ?? 0` would otherwise render "0 traces" over a full
    /// page of results.
    #[test]
    fn trace_summaries_response_omits_total_when_not_requested() {
        let json = serde_json::to_value(TraceSummariesResponse {
            data: vec![],
            total: None,
        })
        .expect("serialize");

        assert!(
            json.get("total").is_none(),
            "absent total must mean 'not computed', never zero: {json}"
        );
    }

    #[test]
    fn trace_summaries_response_includes_total_when_computed() {
        let json = serde_json::to_value(TraceSummariesResponse {
            data: vec![],
            total: Some(0),
        })
        .expect("serialize");

        assert_eq!(
            json.get("total").and_then(|t| t.as_u64()),
            Some(0),
            "a genuinely-zero total must still be serialized: {json}"
        );
    }

    #[test]
    fn cross_project_trace_error_internal_variants_do_not_leak_db_error_text() {
        let err = CrossProjectTraceError::Database(sea_orm::DbErr::Custom(
            "column \"trace_id\" not found in table \"cross_project_trace_refs\"".into(),
        ));
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        let detail = problem
            .body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_eq!(detail, "An internal error occurred");
        assert!(
            !detail.contains("cross_project_trace_refs"),
            "detail leaked: {detail}"
        );
    }

    #[test]
    fn cross_project_trace_error_invalid_trace_id_keeps_user_facing_detail() {
        let err = CrossProjectTraceError::InvalidTraceId {
            trace_id: "not-hex".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        let detail = problem
            .body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(detail.contains("not-hex"), "detail: {detail}");
    }

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
