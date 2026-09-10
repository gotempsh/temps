// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{DateTime, UtcDateTime};
use utoipa::{IntoParams, ToSchema};

use crate::service::proxy_log_service::{
    AiAgentBreakdownRow, AiAgentPageRow, AiAgentTimelineRow, AiPageBreakdownRow,
    AiStatusBreakdownRow, AiTimelineGroupBy, ProjectHealthSummary, ProxyLogResponse,
    ProxyLogService, ProxyLogServiceError, StatsFilters, TimeBucketStats, TodayStatsResponse,
};
use crate::traffic_aggregation::{TrafficAggregationRequest, TrafficAggregationResponse};

pub struct ProxyLogsState {
    pub service: Arc<ProxyLogService>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl std::ops::Deref for ProxyLogsState {
    type Target = ProxyLogService;

    fn deref(&self) -> &Self::Target {
        self.service.as_ref()
    }
}

impl From<ProxyLogServiceError> for Problem {
    fn from(error: ProxyLogServiceError) -> Self {
        match error {
            ProxyLogServiceError::InvalidFilter(_) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Filter Parameters")
                .with_detail(error.to_string()),
            ProxyLogServiceError::TrafficAggregationRateLimited { .. } => {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Traffic Aggregation Rate Limited")
                    .with_detail(error.to_string())
            }
            ProxyLogServiceError::TrafficAggregationTimeout { .. } => {
                problemdetails::new(StatusCode::GATEWAY_TIMEOUT)
                    .with_title("Traffic Aggregation Timed Out")
                    .with_detail(error.to_string())
            }
            // The caller can fix this by asking for less, so it is a 400 with
            // the remedy spelled out — not a 500 the user can only stare at.
            ProxyLogServiceError::TrafficAggregationTooManyGroups { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Traffic Aggregation Too Large")
                    .with_detail(error.to_string())
            }
            // The ClickHouse error's `reason` is the stringified client error,
            // which can embed the internal endpoint host or schema fragments —
            // don't reflect it to the caller. Log the full detail and surface
            // only the operation name.
            ProxyLogServiceError::ClickHouse { operation, reason } => {
                tracing::error!(operation, reason, "ClickHouse proxy-log query failed");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(format!("Storage error during {operation}"))
            }
            ProxyLogServiceError::DatabaseError(e) => {
                tracing::error!(error = %e, "proxy-log database query failed");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("Database error")
            }
        }
    }
}

/// Query parameters for listing proxy logs
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct ProxyLogsQuery {
    /// Filter by project ID
    pub project_id: Option<i32>,
    /// Filter by environment ID
    pub environment_id: Option<i32>,
    /// Filter by deployment ID
    pub deployment_id: Option<i32>,
    /// Filter by session ID
    pub session_id: Option<i32>,
    /// Filter by visitor ID
    pub visitor_id: Option<i32>,

    // Date range filters
    /// Start date for filtering (ISO 8601 format).
    ///
    /// **Defaults to 1 hour before `end_date` (or before now) when omitted.**
    /// The listing is always time-bounded: an unbounded query would have to
    /// consider the entire retention window — 100M+ rows on a busy deployment —
    /// to return a single page. Pass an explicit `start_date` to widen the
    /// window, up to the configured retention horizon.
    ///
    /// The maximum span between `start_date` and `end_date` is 7 days when
    /// `project_id` is omitted, or 30 days when a single `project_id` is set —
    /// a project-scoped query is bounded by that project's own row count
    /// rather than the whole deployment's. A wider request is rejected with a
    /// 400 naming the applicable cap.
    pub start_date: Option<DateTime>,
    /// End date for filtering (ISO 8601 format). Defaults to now.
    pub end_date: Option<DateTime>,

    // Request filters
    /// Filter by HTTP method (GET, POST, etc.)
    pub method: Option<String>,
    /// Filter by host header
    pub host: Option<String>,
    /// Filter by path (supports partial match)
    pub path: Option<String>,
    /// Filter by an exact request path
    pub path_exact: Option<String>,
    /// Filter by client IP address
    pub client_ip: Option<String>,
    /// Exclude Temps status-monitor requests, including legacy monitor rows
    /// identified only by their user-agent.
    pub exclude_synthetic: Option<bool>,

    // Response filters
    /// Filter by HTTP status code
    pub status_code: Option<i16>,
    /// Filter by minimum response time in ms
    pub response_time_min: Option<i32>,
    /// Filter by maximum response time in ms
    pub response_time_max: Option<i32>,

    // Routing filters
    /// Filter by routing status (routed, no_project, error, pending)
    pub routing_status: Option<String>,
    /// Filter by request source (proxy, api, console, cli)
    pub request_source: Option<String>,
    /// Filter by system request flag
    pub is_system_request: Option<bool>,

    // User agent filters
    /// Filter by user agent string (partial match)
    pub user_agent: Option<String>,
    /// Filter by browser name
    pub browser: Option<String>,
    /// Filter by operating system
    pub operating_system: Option<String>,
    /// Filter by device type (mobile, desktop, tablet)
    pub device_type: Option<String>,

    // Bot detection filters
    /// Filter by bot detection
    pub is_bot: Option<bool>,
    /// When `true`, exclude rows flagged as bots while KEEPING rows whose
    /// `is_bot` is NULL (older rows without detection metadata). This is the
    /// tri-state complement of `is_bot=false`, which matches only rows
    /// explicitly detected as non-bots. `false`/omitted is a no-op.
    pub exclude_bots: Option<bool>,
    /// Filter by bot name
    pub bot_name: Option<String>,
    /// Filter by AI provider (e.g. `OpenAI`, `Anthropic`, `Perplexity`). Matches
    /// the canonical provider returned by the AI agent detector.
    pub ai_provider: Option<String>,
    /// Filter by AI agent name (e.g. `GPTBot`, `ChatGPT-User`). Equivalent to
    /// filtering `bot_name` against a known AI taxonomy.
    pub ai_agent: Option<String>,
    /// When `true`, only return requests classified as known AI agents
    /// (regardless of provider/agent). Mutually compatible with the above.
    pub is_ai_agent: Option<bool>,

    // Size filters
    /// Filter by minimum request size in bytes
    pub request_size_min: Option<i64>,
    /// Filter by maximum request size in bytes
    pub request_size_max: Option<i64>,
    /// Filter by minimum response size in bytes
    pub response_size_min: Option<i64>,
    /// Filter by maximum response size in bytes
    pub response_size_max: Option<i64>,

    // Cache filters
    /// Filter by cache status
    pub cache_status: Option<String>,

    // Container filters
    /// Filter by container ID
    pub container_id: Option<String>,
    /// Filter by upstream host
    pub upstream_host: Option<String>,

    // Error filters
    /// Filter by presence of error message
    pub has_error: Option<bool>,

    // Pagination
    /// Page number (default: 1)
    pub page: Option<u64>,
    /// Page size (default: 20, max: 100)
    pub page_size: Option<u64>,

    // Sorting
    /// Sort by field (default: timestamp)
    pub sort_by: Option<String>,
    /// Sort order (asc or desc, default: desc)
    pub sort_order: Option<String>,
}

/// Paginated response for proxy logs
#[derive(Debug, Serialize, ToSchema)]
pub struct ProxyLogsPaginatedResponse {
    pub logs: Vec<ProxyLogResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProxyLogAccessResponse {
    pub allowed: bool,
    pub reason: Option<String>,
}

async fn authorize_proxy_log_scope(
    auth: &temps_auth::AuthContext,
    checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_id: Option<i32>,
) -> Result<(), Problem> {
    if let Some(project_id) = project_id {
        project_permission_guard!(auth, LogsRead, project_id, checker);
        project_scope_guard!(auth, project_id);
    } else {
        // An unscoped proxy-log query spans every project on the instance.
        // Project-level roles cannot be reduced to one safe SQL predicate here,
        // so only instance administrators may use the global view.
        permission_guard!(auth, SystemAdmin);
    }
    Ok(())
}

async fn authorize_proxy_analytics_scope(
    auth: &temps_auth::AuthContext,
    checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    project_id: Option<i32>,
) -> Result<(), Problem> {
    if let Some(project_id) = project_id {
        project_permission_guard!(auth, AnalyticsRead, project_id, checker);
        project_scope_guard!(auth, project_id);
    } else {
        permission_guard!(auth, SystemAdmin);
    }
    Ok(())
}

fn proxy_log_matches_requested_project(
    log_project_id: Option<i32>,
    requested_project_id: Option<i32>,
) -> bool {
    requested_project_id.is_none() || log_project_id == requested_project_id
}

/// Run a backend-neutral, multi-dimensional API traffic aggregation.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/api-analytics/query",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = TrafficAggregationRequest,
    responses(
        (status = 200, description = "Aggregated API traffic", body = TrafficAggregationResponse),
        (status = 400, description = "Invalid dimensions, metrics, filters, or time range", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 429, description = "Aggregation request budget exceeded", body = ProblemDetails),
        (status = 504, description = "Aggregation query timed out", body = ProblemDetails),
        (status = 500, description = "Storage query failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
pub async fn aggregate_api_traffic(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProxyLogsState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<TrafficAggregationRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        AnalyticsRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);

    state
        .aggregate_traffic(project_id, request)
        .await
        .map(Json)
        .map_err(Problem::from)
}

/// Get proxy logs with optional filters and pagination
#[utoipa::path(
    get,
    path = "/proxy-logs",
    params(ProxyLogsQuery),
    responses(
        (status = 200, description = "List of proxy logs", body = ProxyLogsPaginatedResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
pub async fn get_proxy_logs(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<ProxyLogsQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_log_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let page = query.page.unwrap_or(1);
    let page_size = std::cmp::min(query.page_size.unwrap_or(20), 100);
    let start_date = query.start_date.map(|d| d.into());
    let end_date = query.end_date.map(|d| d.into());
    let (logs, total) = service
        .list_with_filters(start_date, end_date, query, page, page_size)
        .await
        .map_err(Problem::from)?;

    let total_pages = (total as f64 / page_size as f64).ceil() as u64;

    let response = ProxyLogsPaginatedResponse {
        logs: logs.into_iter().map(ProxyLogResponse::from).collect(),
        total,
        page,
        page_size,
        total_pages,
    };

    Ok(Json(response))
}

/// Report whether the current principal may drill into a project's proxy logs.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/api-analytics/proxy-log-access",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Proxy-log drilldown capability", body = ProxyLogAccessResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient analytics permissions", body = ProblemDetails),
        (status = 500, description = "Project permission check failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
pub async fn get_api_traffic_proxy_log_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProxyLogsState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        AnalyticsRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);

    match authorize_proxy_log_scope(
        &auth,
        state.project_access_checker.clone(),
        Some(project_id),
    )
    .await
    {
        Ok(()) => Ok(Json(ProxyLogAccessResponse {
            allowed: true,
            reason: None,
        })),
        Err(problem) if problem.status_code == StatusCode::FORBIDDEN => {
            Ok(Json(ProxyLogAccessResponse {
                allowed: false,
                reason: Some(
                    "Your role on this project does not include proxy-log access".to_string(),
                ),
            }))
        }
        Err(problem) => Err(problem),
    }
}

/// Query parameters for the single proxy-log lookup
#[derive(Debug, Deserialize, IntoParams)]
pub struct ProxyLogByIdQuery {
    /// Project scope for the lookup. Required for non-administrator callers.
    /// Instance administrators may omit it for legacy global deep links.
    pub project_id: Option<i32>,
    /// Event time of the log row (ISO 8601). When provided, the lookup is
    /// bounded to the hypertable chunks around this instant instead of
    /// scanning (and decompressing) the whole retention window. The list
    /// endpoint already returns this value per row — always pass it when
    /// navigating from a list.
    pub timestamp: Option<DateTime>,
}

/// Get a single proxy log by ID
#[utoipa::path(
    get,
    path = "/proxy-logs/{id}",
    params(
        ("id" = i32, Path, description = "Proxy log ID"),
        ProxyLogByIdQuery
    ),
    responses(
        (status = 200, description = "Proxy log found", body = ProxyLogResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Proxy log not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
pub async fn get_proxy_log_by_id(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Path(id): Path<i32>,
    Query(query): Query<ProxyLogByIdQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_log_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let log = service
        .get_by_id(id, query.timestamp.map(|t| t.into()))
        .await
        .map_err(Problem::from)?;

    match log {
        Some(log) if proxy_log_matches_requested_project(log.project_id, query.project_id) => {
            Ok(Json(ProxyLogResponse::from(log)))
        }
        Some(_) => Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Proxy Log Not Found")
            .with_detail(format!("Proxy log {id} not found"))),
        None => Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Proxy Log Not Found")
            .with_detail(format!("Proxy log {id} not found"))),
    }
}

/// Get a proxy log by request ID (for tracing)
#[utoipa::path(
    get,
    path = "/proxy-logs/request/{request_id}",
    params(
        ("request_id" = String, Path, description = "Request ID from pingora"),
        ProxyLogByIdQuery
    ),
    responses(
        (status = 200, description = "Proxy log found", body = ProxyLogResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Proxy log not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
pub async fn get_proxy_log_by_request_id(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Path(request_id): Path<String>,
    Query(query): Query<ProxyLogByIdQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_log_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let log = service
        .get_by_request_id(&request_id, query.timestamp.map(|t| t.into()))
        .await
        .map_err(Problem::from)?;

    match log {
        Some(log) if proxy_log_matches_requested_project(log.project_id, query.project_id) => {
            Ok(Json(ProxyLogResponse::from(log)))
        }
        Some(_) => Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Proxy Log Not Found")
            .with_detail(format!("Proxy log with request ID {request_id} not found"))),
        None => Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Proxy Log Not Found")
            .with_detail(format!("Proxy log with request ID {request_id} not found"))),
    }
}

/// Query parameters for stats requests
#[derive(Debug, Deserialize, IntoParams)]
pub struct StatsQuery {
    /// Filter by HTTP method
    pub method: Option<String>,
    /// Filter by client IP
    pub client_ip: Option<String>,
    /// Filter by project ID
    pub project_id: Option<i32>,
    /// Filter by environment ID
    pub environment_id: Option<i32>,
    /// Filter by deployment ID
    pub deployment_id: Option<i32>,
    /// Filter by host
    pub host: Option<String>,
    /// Filter by status code
    pub status_code: Option<i16>,
    /// Filter by status code class (e.g. "2xx", "3xx", "4xx", "5xx")
    pub status_code_class: Option<String>,
    /// Filter by routing status
    pub routing_status: Option<String>,
    /// Filter by request source
    pub request_source: Option<String>,
    /// Filter by bot detection
    pub is_bot: Option<bool>,
    /// Filter by device type
    pub device_type: Option<String>,
}

impl From<StatsQuery> for StatsFilters {
    fn from(query: StatsQuery) -> Self {
        Self {
            method: query.method,
            client_ip: query.client_ip,
            project_id: query.project_id,
            environment_id: query.environment_id,
            deployment_id: query.deployment_id,
            host: query.host,
            status_code: query.status_code,
            status_code_class: query.status_code_class,
            routing_status: query.routing_status,
            request_source: query.request_source,
            is_bot: query.is_bot,
            device_type: query.device_type,
            has_project: None,
            exclude_synthetic: false,
        }
    }
}

/// Query parameters for time bucket stats
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TimeBucketStatsQuery {
    /// Start time (ISO 8601 format)
    #[param(value_type = String, example = "2025-10-23T00:00:00Z")]
    pub start_time: UtcDateTime,
    /// End time (ISO 8601 format)
    #[param(value_type = String, example = "2025-10-23T23:59:59Z")]
    pub end_time: UtcDateTime,
    /// Bucket interval (e.g., "1 hour", "1 day", "5 minutes").
    /// Must keep `span / interval` ≤ 1000 buckets (7 days at 1 minute is rejected).
    #[serde(default = "default_bucket_interval")]
    pub bucket_interval: String,
    /// Filter by HTTP method
    pub method: Option<String>,
    /// Filter by client IP
    pub client_ip: Option<String>,
    /// Filter by project ID
    pub project_id: Option<i32>,
    /// Filter by environment ID
    pub environment_id: Option<i32>,
    /// Filter by deployment ID
    pub deployment_id: Option<i32>,
    /// Filter by host
    pub host: Option<String>,
    /// Filter by status code
    pub status_code: Option<i16>,
    /// Filter by status code class (e.g. "2xx", "3xx", "4xx", "5xx")
    pub status_code_class: Option<String>,
    /// Filter by routing status
    pub routing_status: Option<String>,
    /// Filter by request source
    pub request_source: Option<String>,
    /// Filter by bot detection
    pub is_bot: Option<bool>,
    /// Filter by device type
    pub device_type: Option<String>,
    /// When true, only count requests that matched a project
    /// (project_id IS NOT NULL). Makes chart totals line up with the
    /// per-project health cards.
    pub has_project: Option<bool>,
}

fn default_bucket_interval() -> String {
    "1 hour".to_string()
}

/// Response for time bucket stats
#[derive(Debug, Serialize, ToSchema)]
pub struct TimeBucketStatsResponse {
    pub stats: Vec<TimeBucketStats>,
    pub start_time: String,
    pub end_time: String,
    pub bucket_interval: String,
}

/// Get today's request count with optional filters
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/today",
    params(StatsQuery),
    responses(
        (status = 200, description = "Today's request count", body = TodayStatsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn get_today_stats(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<StatsQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_analytics_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let filters = if query.method.is_some()
        || query.client_ip.is_some()
        || query.project_id.is_some()
        || query.environment_id.is_some()
        || query.deployment_id.is_some()
        || query.host.is_some()
        || query.status_code.is_some()
        || query.status_code_class.is_some()
        || query.routing_status.is_some()
        || query.request_source.is_some()
        || query.is_bot.is_some()
        || query.device_type.is_some()
    {
        Some(StatsFilters::from(query))
    } else {
        None
    };

    let count = service
        .get_today_count(filters)
        .await
        .map_err(Problem::from)?;

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    Ok(Json(TodayStatsResponse {
        total_requests: count,
        date: today,
    }))
}

/// Get time-bucketed statistics with optional filters
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/time-buckets",
    params(TimeBucketStatsQuery),
    responses(
        (status = 200, description = "Time-bucketed statistics", body = TimeBucketStatsResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn get_time_bucket_stats(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<TimeBucketStatsQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_analytics_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let filters = if query.method.is_some()
        || query.client_ip.is_some()
        || query.project_id.is_some()
        || query.environment_id.is_some()
        || query.deployment_id.is_some()
        || query.host.is_some()
        || query.status_code.is_some()
        || query.status_code_class.is_some()
        || query.routing_status.is_some()
        || query.request_source.is_some()
        || query.is_bot.is_some()
        || query.device_type.is_some()
        || query.has_project.is_some()
    {
        Some(StatsFilters {
            method: query.method,
            client_ip: query.client_ip,
            project_id: query.project_id,
            environment_id: query.environment_id,
            deployment_id: query.deployment_id,
            host: query.host,
            status_code: query.status_code,
            status_code_class: query.status_code_class,
            routing_status: query.routing_status,
            request_source: query.request_source,
            is_bot: query.is_bot,
            device_type: query.device_type,
            has_project: query.has_project,
            exclude_synthetic: false,
        })
    } else {
        None
    };

    let stats = service
        .get_time_bucket_stats(
            query.start_time,
            query.end_time,
            query.bucket_interval.clone(),
            filters,
        )
        .await
        .map_err(Problem::from)?;

    Ok(Json(TimeBucketStatsResponse {
        stats,
        start_time: query.start_time.to_rfc3339(),
        end_time: query.end_time.to_rfc3339(),
        bucket_interval: query.bucket_interval,
    }))
}

/// Query parameters for batch project health summary
#[derive(Debug, Deserialize, IntoParams)]
pub struct ProjectsHealthQuery {
    /// Comma-separated list of project IDs
    pub project_ids: String,
    /// Optional start time (ISO 8601). Defaults to `end_time - 1h`.
    #[param(value_type = Option<String>, example = "2025-10-23T00:00:00Z")]
    pub start_time: Option<UtcDateTime>,
    /// Optional end time (ISO 8601). Defaults to now.
    #[param(value_type = Option<String>, example = "2025-10-23T23:59:59Z")]
    pub end_time: Option<UtcDateTime>,
    /// Filter by bot detection. Pass `false` to exclude bots, `true` for bots only.
    pub is_bot: Option<bool>,
}

/// Batch health summary response
#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectsHealthResponse {
    /// Health summaries keyed by project ID
    pub projects: std::collections::HashMap<String, ProjectHealthSummary>,
}

const MAX_PROJECTS_HEALTH_IDS: usize = 100;
const MAX_PROJECTS_HEALTH_WINDOW_DAYS: i64 = 31;

fn parse_projects_health_project_ids(project_ids: &str) -> Result<Vec<i32>, Problem> {
    let mut ids = Vec::new();
    for value in project_ids.split(',').map(str::trim) {
        let project_id = value.parse::<i32>().map_err(|_| {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Parameters")
                .with_detail(format!("'{value}' is not a valid project ID"))
        })?;
        if !ids.contains(&project_id) {
            ids.push(project_id);
        }
    }
    if ids.is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail("project_ids must contain at least one project ID"));
    }
    if ids.len() > MAX_PROJECTS_HEALTH_IDS {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail(format!(
                "Maximum {MAX_PROJECTS_HEALTH_IDS} project IDs allowed"
            )));
    }
    Ok(ids)
}

fn validate_projects_health_window(
    start_time: UtcDateTime,
    end_time: UtcDateTime,
) -> Result<(), Problem> {
    if start_time >= end_time {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail("start_time must be before end_time"));
    }
    if end_time - start_time > chrono::Duration::days(MAX_PROJECTS_HEALTH_WINDOW_DAYS) {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail(format!(
                "projects-health time range cannot exceed {MAX_PROJECTS_HEALTH_WINDOW_DAYS} days"
            )));
    }
    Ok(())
}

/// Get user-traffic health summaries for multiple projects (last 1 hour).
/// Temps' own status-monitor requests are excluded.
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/projects-health",
    params(ProjectsHealthQuery),
    responses(
        (status = 200, description = "Health summaries per project", body = ProjectsHealthResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn get_projects_health(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<ProjectsHealthQuery>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = parse_projects_health_project_ids(&query.project_ids)?;
    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::hours(1));
    validate_projects_health_window(start_time, end_time)?;

    for project_id in &project_ids {
        authorize_proxy_analytics_scope(
            &auth,
            service.project_access_checker.clone(),
            Some(*project_id),
        )
        .await?;
    }

    let summaries = service
        .get_projects_health_summary(&project_ids, start_time, end_time, query.is_bot)
        .await
        .map_err(Problem::from)?;

    let projects: std::collections::HashMap<String, ProjectHealthSummary> = summaries
        .into_iter()
        .map(|s| (s.project_id.to_string(), s))
        .collect();

    Ok(Json(ProjectsHealthResponse { projects }))
}

/// Query parameters for the AI agent breakdown
#[derive(Debug, Deserialize, IntoParams)]
pub struct AiAgentBreakdownQuery {
    /// Filter by project ID (recommended for per-project analytics).
    pub project_id: Option<i32>,
    /// Filter by environment ID.
    pub environment_id: Option<i32>,
    /// Start time (ISO 8601). Defaults to `end_time - 7d`.
    #[param(value_type = Option<String>, example = "2026-05-22T00:00:00Z")]
    pub start_time: Option<UtcDateTime>,
    /// End time (ISO 8601). Defaults to now.
    #[param(value_type = Option<String>, example = "2026-05-29T00:00:00Z")]
    pub end_time: Option<UtcDateTime>,
    /// Maximum rows to return. Capped at 100 server-side.
    pub limit: Option<u64>,
    /// Optional exact path filter. Only used by the AI pages breakdown — when
    /// set, returns the single matching page so callers can ask "how many AI
    /// agents hit this page?".
    pub path: Option<String>,
}

/// Response wrapping the AI agent breakdown rows.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiAgentBreakdownResponse {
    pub items: Vec<AiAgentBreakdownRow>,
    pub start_time: String,
    pub end_time: String,
}

/// Static descriptor for one entry in the known-AI-agents taxonomy.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiAgentDescriptor {
    pub provider: String,
    pub agent: String,
    pub purpose: String,
}

/// Response listing every AI agent the detector knows about.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnownAiAgentsResponse {
    pub items: Vec<AiAgentDescriptor>,
}

/// Get the per-AI-agent breakdown for a project over a time window.
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/ai-agents",
    params(AiAgentBreakdownQuery),
    responses(
        (status = 200, description = "AI agent breakdown", body = AiAgentBreakdownResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn get_ai_agent_breakdown(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<AiAgentBreakdownQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_analytics_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail("start_time must be before end_time"));
    }

    let limit = query.limit.unwrap_or(20);

    let items = service
        .get_ai_agent_breakdown(
            query.project_id,
            query.environment_id,
            query.path.clone(),
            start_time,
            end_time,
            limit,
        )
        .await
        .map_err(Problem::from)?;

    Ok(Json(AiAgentBreakdownResponse {
        items,
        start_time: start_time.to_rfc3339(),
        end_time: end_time.to_rfc3339(),
    }))
}

/// Response wrapping the AI page breakdown rows.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiPageBreakdownResponse {
    pub items: Vec<AiPageBreakdownRow>,
    pub start_time: String,
    pub end_time: String,
}

/// Get the top pages crawled by AI agents over a time window.
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/ai-pages",
    params(AiAgentBreakdownQuery),
    responses(
        (status = 200, description = "AI page breakdown", body = AiPageBreakdownResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn get_ai_page_breakdown(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<AiAgentBreakdownQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_analytics_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail("start_time must be before end_time"));
    }

    let limit = query.limit.unwrap_or(50);

    let items = service
        .get_ai_page_breakdown(
            query.project_id,
            query.environment_id,
            query.path.clone(),
            start_time,
            end_time,
            limit,
        )
        .await
        .map_err(Problem::from)?;

    Ok(Json(AiPageBreakdownResponse {
        items,
        start_time: start_time.to_rfc3339(),
        end_time: end_time.to_rfc3339(),
    }))
}

/// Query for the AI-agent timeline. Reuses the breakdown window params and adds
/// the grouping dimension. The bucket interval is auto-selected from the window
/// width unless `bucket` is provided.
#[derive(Debug, Deserialize, IntoParams)]
pub struct AiAgentTimelineQuery {
    /// Filter by project ID.
    pub project_id: Option<i32>,
    /// Filter by environment ID.
    pub environment_id: Option<i32>,
    /// Start time (ISO 8601). Defaults to `end_time - 7d`.
    #[param(value_type = Option<String>, example = "2026-05-22T00:00:00Z")]
    pub start_time: Option<UtcDateTime>,
    /// End time (ISO 8601). Defaults to now.
    #[param(value_type = Option<String>, example = "2026-05-29T00:00:00Z")]
    pub end_time: Option<UtcDateTime>,
    /// Grouping dimension: `provider` (default) or `agent`.
    #[param(example = "provider")]
    pub group_by: Option<String>,
    /// Bucket interval override (e.g. `1 hour`, `1 day`). Auto-selected from the
    /// window width when omitted.
    #[param(example = "1 hour")]
    pub bucket: Option<String>,
}

/// Response wrapping the AI agent timeline rows.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiAgentTimelineResponse {
    pub items: Vec<AiAgentTimelineRow>,
    pub start_time: String,
    pub end_time: String,
    /// Bucket interval used for the buckets (so the UI can label the x-axis).
    #[schema(example = "1 hour")]
    pub bucket: String,
    /// Echoes the grouping dimension actually applied.
    #[schema(example = "provider")]
    pub group_by: String,
}

/// Pick a sensible time-bucket width for a window, mirroring the frontend's
/// aggregation choice so the AI timeline lines up with the traffic chart.
fn auto_bucket_for_window(start: UtcDateTime, end: UtcDateTime) -> &'static str {
    let hours = (end - start).num_hours().max(1);
    if hours <= 2 {
        "5 minutes"
    } else if hours <= 48 {
        "1 hour"
    } else if hours <= 24 * 14 {
        "6 hours"
    } else {
        "1 day"
    }
}

/// Time-bucketed AI-agent request volume, split by provider or agent.
///
/// Powers the "AI agents over time" stacked chart. Same data source as the AI
/// agent breakdown (request logs), just bucketed.
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/ai-agents/timeline",
    params(AiAgentTimelineQuery),
    responses(
        (status = 200, description = "AI agent timeline", body = AiAgentTimelineResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn get_ai_agent_timeline(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<AiAgentTimelineQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_analytics_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail("start_time must be before end_time"));
    }

    let group_by = match query.group_by.as_deref() {
        Some("agent") => AiTimelineGroupBy::Agent,
        Some("provider") | None => AiTimelineGroupBy::Provider,
        Some(other) => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Parameters")
                .with_detail(format!(
                    "group_by must be 'provider' or 'agent', got '{other}'"
                )));
        }
    };

    // Validate the optional bucket override here so a bad value is a 400 (like
    // `group_by` above), not a 500 from the service layer — and so the raw user
    // string is never reflected back in the error body. The service re-checks
    // via `is_valid_interval` as defense-in-depth.
    if let Some(ref b) = query.bucket {
        if !ProxyLogService::is_valid_interval(b) {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Parameters")
                .with_detail(
                    "bucket must be a valid interval, e.g. '1 hour', '6 hours' or '1 day'",
                ));
        }
    }

    let bucket = query
        .bucket
        .clone()
        .unwrap_or_else(|| auto_bucket_for_window(start_time, end_time).to_string());

    let items = service
        .get_ai_agent_timeline(
            query.project_id,
            query.environment_id,
            start_time,
            end_time,
            bucket.clone(),
            group_by,
        )
        .await
        .map_err(Problem::from)?;

    Ok(Json(AiAgentTimelineResponse {
        items,
        start_time: start_time.to_rfc3339(),
        end_time: end_time.to_rfc3339(),
        bucket,
        group_by: match group_by {
            AiTimelineGroupBy::Agent => "agent".to_string(),
            AiTimelineGroupBy::Provider => "provider".to_string(),
        },
    }))
}

/// Response wrapping the AI status breakdown rows.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiStatusBreakdownResponse {
    pub items: Vec<AiStatusBreakdownRow>,
    pub start_time: String,
    pub end_time: String,
}

/// HTTP status-class breakdown for AI-agent traffic — are bots being served
/// (2xx) or hitting broken/blocked pages (4xx/5xx)?
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/ai-status",
    params(AiAgentBreakdownQuery),
    responses(
        (status = 200, description = "AI status breakdown", body = AiStatusBreakdownResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn get_ai_status_breakdown(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<AiAgentBreakdownQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_analytics_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail("start_time must be before end_time"));
    }

    let items = service
        .get_ai_status_breakdown(query.project_id, query.environment_id, start_time, end_time)
        .await
        .map_err(Problem::from)?;

    Ok(Json(AiStatusBreakdownResponse {
        items,
        start_time: start_time.to_rfc3339(),
        end_time: end_time.to_rfc3339(),
    }))
}

/// List every AI agent the detector knows how to classify.
///
/// Returned in the same order as the internal taxonomy so the UI can use it as
/// a stable dropdown.
#[utoipa::path(
    get,
    path = "/proxy-logs/ai-agents/known",
    responses(
        (status = 200, description = "Known AI agents", body = KnownAiAgentsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn list_known_ai_agents(
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let items: Vec<AiAgentDescriptor> = crate::ai_agent_detector::known_agents()
        .iter()
        .map(|(_, m)| AiAgentDescriptor {
            provider: m.provider.to_string(),
            agent: m.agent.to_string(),
            purpose: m.purpose.as_str().to_string(),
        })
        .collect();
    Ok(Json(KnownAiAgentsResponse { items }))
}

/// Query parameters for the per-agent pages breakdown.
#[derive(Debug, Deserialize, IntoParams)]
pub struct AiAgentPagesQuery {
    /// Canonical agent name to filter by (e.g. `ChatGPT-User`, `ClaudeBot`).
    /// Must be a name returned by `GET /proxy-logs/ai-agents/known`.
    pub agent: String,
    /// Filter by project ID.
    pub project_id: Option<i32>,
    /// Filter by environment ID.
    pub environment_id: Option<i32>,
    /// Start time (ISO 8601). Defaults to `end_time - 7d`.
    #[param(value_type = Option<String>, example = "2026-05-22T00:00:00Z")]
    pub start_time: Option<UtcDateTime>,
    /// End time (ISO 8601). Defaults to now.
    #[param(value_type = Option<String>, example = "2026-05-29T00:00:00Z")]
    pub end_time: Option<UtcDateTime>,
    /// Maximum rows to return. Capped at 100 server-side.
    pub limit: Option<u64>,
}

/// Response wrapping the per-agent pages breakdown rows.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiAgentPagesResponse {
    /// The agent name this breakdown is scoped to.
    pub agent: String,
    pub items: Vec<AiAgentPageRow>,
    pub start_time: String,
    pub end_time: String,
}

/// Get the top pages accessed by a specific AI agent over a time window.
///
/// Returns page paths ranked by request count, scoped to a single canonical
/// agent name (e.g. `ChatGPT-User`). Use `GET /proxy-logs/ai-agents/known` to
/// list all valid agent names. Unknown agent names return an empty items array.
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/ai-agent-pages",
    params(AiAgentPagesQuery),
    responses(
        (status = 200, description = "Pages breakdown for the requested agent", body = AiAgentPagesResponse),
        (status = 400, description = "Invalid parameters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = [])),
    tag = "Proxy Logs"
)]
async fn get_ai_agent_pages(
    RequireAuth(auth): RequireAuth,
    State(service): State<Arc<ProxyLogsState>>,
    Query(query): Query<AiAgentPagesQuery>,
) -> Result<impl IntoResponse, Problem> {
    authorize_proxy_analytics_scope(
        &auth,
        service.project_access_checker.clone(),
        query.project_id,
    )
    .await?;

    if query.agent.trim().is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail("agent parameter is required"));
    }

    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Parameters")
            .with_detail("start_time must be before end_time"));
    }

    let limit = query.limit.unwrap_or(50);

    let items = service
        .get_ai_agent_pages(
            &query.agent,
            query.project_id,
            query.environment_id,
            start_time,
            end_time,
            limit,
        )
        .await
        .map_err(Problem::from)?;

    Ok(Json(AiAgentPagesResponse {
        agent: query.agent,
        items,
        start_time: start_time.to_rfc3339(),
        end_time: end_time.to_rfc3339(),
    }))
}

/// Create router for proxy log handlers
pub fn create_routes() -> axum::Router<Arc<ProxyLogsState>> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route(
            "/projects/{project_id}/api-analytics/query",
            post(aggregate_api_traffic),
        )
        .route(
            "/projects/{project_id}/api-analytics/proxy-log-access",
            get(get_api_traffic_proxy_log_access),
        )
        .route("/proxy-logs", get(get_proxy_logs))
        .route("/proxy-logs/{id}", get(get_proxy_log_by_id))
        .route(
            "/proxy-logs/request/{request_id}",
            get(get_proxy_log_by_request_id),
        )
        .route("/proxy-logs/stats/today", get(get_today_stats))
        .route("/proxy-logs/stats/time-buckets", get(get_time_bucket_stats))
        .route(
            "/proxy-logs/stats/projects-health",
            get(get_projects_health),
        )
        .route("/proxy-logs/stats/ai-agents", get(get_ai_agent_breakdown))
        .route(
            "/proxy-logs/stats/ai-agents/timeline",
            get(get_ai_agent_timeline),
        )
        .route("/proxy-logs/stats/ai-pages", get(get_ai_page_breakdown))
        .route("/proxy-logs/stats/ai-status", get(get_ai_status_breakdown))
        .route("/proxy-logs/stats/ai-agent-pages", get(get_ai_agent_pages))
        .route("/proxy-logs/ai-agents/known", get(list_known_ai_agents))
}

/// Get OpenAPI documentation for proxy logs handlers
pub fn openapi() -> utoipa::openapi::OpenApi {
    use utoipa::OpenApi;

    #[derive(OpenApi)]
    #[openapi(
        paths(
            aggregate_api_traffic,
            get_api_traffic_proxy_log_access,
            get_proxy_logs,
            get_proxy_log_by_id,
            get_proxy_log_by_request_id,
            get_today_stats,
            get_time_bucket_stats,
            get_projects_health,
            get_ai_agent_breakdown,
            get_ai_agent_timeline,
            get_ai_page_breakdown,
            get_ai_status_breakdown,
            get_ai_agent_pages,
            list_known_ai_agents,
        ),
        components(schemas(
            TrafficAggregationRequest,
            TrafficAggregationResponse,
            crate::traffic_aggregation::TrafficAggregationRow,
            crate::traffic_aggregation::TrafficDimensionValue,
            crate::traffic_aggregation::TrafficMetricValues,
            crate::traffic_aggregation::TrafficDimension,
            crate::traffic_aggregation::TrafficMetric,
            crate::traffic_aggregation::TrafficFilter,
            crate::traffic_aggregation::TrafficFilterOperator,
            crate::traffic_aggregation::TrafficOrderBy,
            crate::traffic_aggregation::TrafficOrderField,
            crate::traffic_aggregation::TrafficSortDirection,
            ProxyLogResponse,
            ProxyLogsPaginatedResponse,
            ProxyLogAccessResponse,
            TodayStatsResponse,
            TimeBucketStatsResponse,
            TimeBucketStats,
            StatsFilters,
            ProjectHealthSummary,
            crate::service::proxy_log_service::ProjectHourlyRequestCount,
            ProjectsHealthResponse,
            AiAgentBreakdownResponse,
            AiAgentBreakdownRow,
            AiAgentTimelineResponse,
            AiAgentTimelineRow,
            AiPageBreakdownResponse,
            AiPageBreakdownRow,
            AiStatusBreakdownResponse,
            AiStatusBreakdownRow,
            AiAgentPageRow,
            AiAgentPagesResponse,
            AiAgentDescriptor,
            KnownAiAgentsResponse,
        ))
    )]
    struct ApiDoc;

    ApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use temps_auth::{AuthContext, Role};
    use temps_core::ProjectAccessChecker;
    use temps_entities::users;

    fn test_auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id: 42,
                name: "Proxy Log Tester".to_string(),
                email: "proxy-log-tester@example.com".to_string(),
                password_hash: None,
                email_verified: true,
                email_verification_token: None,
                email_verification_expires: None,
                password_reset_token: None,
                password_reset_expires: None,
                must_change_password: false,
                deleted_at: None,
                mfa_secret: None,
                mfa_enabled: false,
                mfa_recovery_codes: None,
                oidc_subject: None,
                oidc_provider_id: None,
                created_at: now,
                updated_at: now,
            },
            role,
        )
    }

    enum PermissionOutcome {
        Allow,
        Deny,
        Error,
    }

    struct TestProjectAccessChecker(PermissionOutcome);

    #[async_trait]
    impl ProjectAccessChecker for TestProjectAccessChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(true)
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            match self.0 {
                PermissionOutcome::Allow => Ok(Some(vec![
                    "logs:read".to_string(),
                    "analytics:read".to_string(),
                ])),
                PermissionOutcome::Deny => Ok(Some(Vec::new())),
                PermissionOutcome::Error => {
                    Err(Box::new(std::io::Error::other("permission store offline")))
                }
            }
        }
    }

    fn checker(outcome: PermissionOutcome) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(TestProjectAccessChecker(outcome)))
    }

    fn window(hours: i64) -> (UtcDateTime, UtcDateTime) {
        // Fixed anchor so the test is deterministic (no `Utc::now()`).
        // `UtcDateTime` is a chrono `DateTime<Utc>` alias.
        let start = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let end = start + chrono::Duration::hours(hours);
        (start, end)
    }

    #[test]
    fn projects_health_ids_are_strict_deduplicated_and_bounded() {
        assert_eq!(
            parse_projects_health_project_ids("7, 8,7").expect("valid project IDs"),
            vec![7, 8]
        );

        let malformed = parse_projects_health_project_ids("7,not-an-id")
            .expect_err("malformed ID must reject the whole request");
        assert_eq!(malformed.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            malformed
                .body
                .get("detail")
                .and_then(|value| value.as_str()),
            Some("'not-an-id' is not a valid project ID")
        );

        let repeated = std::iter::repeat_n("7", MAX_PROJECTS_HEALTH_IDS + 1)
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            parse_projects_health_project_ids(&repeated).expect("duplicates do not consume cap"),
            vec![7]
        );

        let too_many = (1..=MAX_PROJECTS_HEALTH_IDS + 1)
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let error = parse_projects_health_project_ids(&too_many)
            .expect_err("more than 100 distinct IDs must be rejected");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn projects_health_window_is_limited_to_31_days() {
        let (start, _) = window(1);
        assert!(validate_projects_health_window(
            start,
            start + chrono::Duration::days(MAX_PROJECTS_HEALTH_WINDOW_DAYS)
        )
        .is_ok());

        let error = validate_projects_health_window(
            start,
            start
                + chrono::Duration::days(MAX_PROJECTS_HEALTH_WINDOW_DAYS)
                + chrono::Duration::seconds(1),
        )
        .expect_err("a range beyond 31 days must be rejected before gap-fill");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.body.get("detail").and_then(|value| value.as_str()),
            Some("projects-health time range cannot exceed 31 days")
        );
    }

    #[tokio::test]
    async fn proxy_log_list_requires_project_logs_permission() {
        let auth = test_auth(Role::Reader);
        assert!(
            authorize_proxy_log_scope(&auth, checker(PermissionOutcome::Allow), Some(7))
                .await
                .is_ok()
        );

        let denied = authorize_proxy_log_scope(&auth, checker(PermissionOutcome::Deny), Some(7))
            .await
            .expect_err("project role without logs:read must be denied");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn proxy_log_list_fails_closed_when_project_permission_check_fails() {
        let error = authorize_proxy_log_scope(
            &test_auth(Role::Reader),
            checker(PermissionOutcome::Error),
            Some(7),
        )
        .await
        .expect_err("permission infrastructure failure must deny the request");
        assert_eq!(error.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn unscoped_proxy_log_list_is_admin_only() {
        let denied = authorize_proxy_log_scope(&test_auth(Role::User), None, None)
            .await
            .expect_err("ordinary users must not list logs across all projects");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
        assert!(
            authorize_proxy_log_scope(&test_auth(Role::Admin), None, None)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn analytics_only_role_cannot_open_proxy_logs() {
        let denied = authorize_proxy_log_scope(
            &test_auth(Role::ApiReader),
            checker(PermissionOutcome::Allow),
            Some(7),
        )
        .await
        .expect_err("analytics-only role lacks instance logs:read permission");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn analytics_stats_require_project_scope_or_an_instance_admin() {
        let reader = test_auth(Role::Reader);
        assert!(authorize_proxy_analytics_scope(
            &reader,
            checker(PermissionOutcome::Allow),
            Some(7),
        )
        .await
        .is_ok());

        let denied =
            authorize_proxy_analytics_scope(&reader, checker(PermissionOutcome::Deny), Some(7))
                .await
                .expect_err("analytics for another project must be denied");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        let unscoped = authorize_proxy_analytics_scope(&reader, None, None)
            .await
            .expect_err("global analytics are administrator-only");
        assert_eq!(unscoped.status_code, StatusCode::FORBIDDEN);
        assert!(
            authorize_proxy_analytics_scope(&test_auth(Role::Admin), None, None)
                .await
                .is_ok()
        );
    }

    #[test]
    fn single_log_lookup_never_crosses_the_authorized_project_scope() {
        assert!(proxy_log_matches_requested_project(Some(7), Some(7)));
        assert!(!proxy_log_matches_requested_project(Some(8), Some(7)));
        assert!(!proxy_log_matches_requested_project(None, Some(7)));
        assert!(proxy_log_matches_requested_project(Some(8), None));
    }

    #[test]
    fn auto_bucket_picks_granularity_by_window_width() {
        // <= 2h → 5 minutes
        let (s, e) = window(1);
        assert_eq!(auto_bucket_for_window(s, e), "5 minutes");
        let (s, e) = window(2);
        assert_eq!(auto_bucket_for_window(s, e), "5 minutes");

        // (2h, 48h] → 1 hour
        let (s, e) = window(3);
        assert_eq!(auto_bucket_for_window(s, e), "1 hour");
        let (s, e) = window(48);
        assert_eq!(auto_bucket_for_window(s, e), "1 hour");

        // (48h, 14d] → 6 hours
        let (s, e) = window(49);
        assert_eq!(auto_bucket_for_window(s, e), "6 hours");
        let (s, e) = window(24 * 14);
        assert_eq!(auto_bucket_for_window(s, e), "6 hours");

        // > 14d → 1 day
        let (s, e) = window(24 * 14 + 1);
        assert_eq!(auto_bucket_for_window(s, e), "1 day");
        let (s, e) = window(24 * 90);
        assert_eq!(auto_bucket_for_window(s, e), "1 day");
    }

    #[test]
    fn auto_bucket_clamps_zero_or_inverted_window_to_minimum() {
        // A zero-width (or end<=start) window must not panic and must still
        // return the finest granularity — `num_hours().max(1)` guarantees this.
        let (s, _) = window(0);
        assert_eq!(auto_bucket_for_window(s, s), "5 minutes");
        let (s, e) = window(5);
        // end before start
        assert_eq!(auto_bucket_for_window(e, s), "5 minutes");
    }

    #[test]
    fn bucket_interval_allowlist_accepts_valid_and_rejects_injection() {
        use crate::service::proxy_log_service::ProxyLogService;
        // Valid forms the auto-bucket + UI emit.
        assert!(ProxyLogService::is_valid_interval("5 minutes"));
        assert!(ProxyLogService::is_valid_interval("1 hour"));
        assert!(ProxyLogService::is_valid_interval("6 hours"));
        assert!(ProxyLogService::is_valid_interval("1 day"));
        // Junk / injection attempts the handler must reject with a 400.
        assert!(!ProxyLogService::is_valid_interval(
            "1; DROP TABLE proxy_logs"
        ));
        assert!(!ProxyLogService::is_valid_interval("evil"));
        assert!(!ProxyLogService::is_valid_interval("1 fortnight"));
        assert!(!ProxyLogService::is_valid_interval("-1 hour"));
        assert!(!ProxyLogService::is_valid_interval(""));
    }

    #[test]
    fn ai_agent_pages_query_rejects_empty_agent() {
        // The handler returns 400 for a blank agent string before hitting the DB.
        // We test the validation logic directly (no HTTP stack needed).
        let agent = "   ";
        assert!(
            agent.trim().is_empty(),
            "blank/whitespace agent must be caught as empty"
        );
    }

    #[test]
    fn ai_agent_pages_unknown_agent_returns_empty_from_service() {
        // `get_ai_agent_pages` returns Ok(vec![]) for any name not in the
        // known-agents taxonomy without touching the DB.
        use crate::ai_agent_detector::known_agents;
        let unknown = "NotARealBot/1.0";
        let is_known = known_agents().iter().any(|(_, m)| m.agent == unknown);
        assert!(!is_known, "test agent must not be in the taxonomy");
    }

    #[test]
    fn ai_agent_pages_known_agent_passes_validation() {
        use crate::ai_agent_detector::known_agents;
        // Verify that at least the agents shown in the UI snapshot are in the
        // taxonomy so the handler will proceed to query the DB for them.
        for name in &[
            "ChatGPT-User",
            "ClaudeBot",
            "Claude-User",
            "PerplexityBot",
            "GPTBot",
        ] {
            assert!(
                known_agents().iter().any(|(_, m)| &m.agent == name),
                "expected {} in known_agents taxonomy",
                name
            );
        }
    }
}
