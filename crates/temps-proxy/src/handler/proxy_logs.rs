use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{DateTime, UtcDateTime};
use utoipa::{IntoParams, ToSchema};

use crate::service::proxy_log_service::{
    AiAgentBreakdownRow, AiAgentTimelineRow, AiPageBreakdownRow, AiStatusBreakdownRow,
    AiTimelineGroupBy, ProjectHealthSummary, ProxyLogResponse, ProxyLogService, StatsFilters,
    TimeBucketStats, TodayStatsResponse,
};

/// Query parameters for listing proxy logs
#[derive(Debug, Deserialize, IntoParams)]
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
    /// Start date for filtering (ISO 8601 format)
    pub start_date: Option<DateTime>,
    /// End date for filtering (ISO 8601 format)
    pub end_date: Option<DateTime>,

    // Request filters
    /// Filter by HTTP method (GET, POST, etc.)
    pub method: Option<String>,
    /// Filter by host header
    pub host: Option<String>,
    /// Filter by path (supports partial match)
    pub path: Option<String>,
    /// Filter by client IP address
    pub client_ip: Option<String>,

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

/// Get proxy logs with optional filters and pagination
#[utoipa::path(
    get,
    path = "/proxy-logs",
    params(ProxyLogsQuery),
    responses(
        (status = 200, description = "List of proxy logs", body = ProxyLogsPaginatedResponse),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
pub async fn get_proxy_logs(
    State(service): State<Arc<ProxyLogService>>,
    Query(query): Query<ProxyLogsQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let page = query.page.unwrap_or(1);
    let page_size = std::cmp::min(query.page_size.unwrap_or(20), 100);
    let start_date = query.start_date.map(|d| d.into());
    let end_date = query.end_date.map(|d| d.into());
    let (logs, total) = service
        .list_with_filters(start_date, end_date, query, page, page_size)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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

/// Get a single proxy log by ID
#[utoipa::path(
    get,
    path = "/proxy-logs/{id}",
    params(
        ("id" = i32, Path, description = "Proxy log ID")
    ),
    responses(
        (status = 200, description = "Proxy log found", body = ProxyLogResponse),
        (status = 404, description = "Proxy log not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
pub async fn get_proxy_log_by_id(
    State(service): State<Arc<ProxyLogService>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let log = service
        .get_by_id(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match log {
        Some(log) => Ok(Json(ProxyLogResponse::from(log))),
        None => Err((StatusCode::NOT_FOUND, "Proxy log not found".to_string())),
    }
}

/// Get a proxy log by request ID (for tracing)
#[utoipa::path(
    get,
    path = "/proxy-logs/request/{request_id}",
    params(
        ("request_id" = String, Path, description = "Request ID from pingora")
    ),
    responses(
        (status = 200, description = "Proxy log found", body = ProxyLogResponse),
        (status = 404, description = "Proxy log not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
pub async fn get_proxy_log_by_request_id(
    State(service): State<Arc<ProxyLogService>>,
    Path(request_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let log = service
        .get_by_request_id(&request_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match log {
        Some(log) => Ok(Json(ProxyLogResponse::from(log))),
        None => Err((StatusCode::NOT_FOUND, "Proxy log not found".to_string())),
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
    /// Bucket interval (e.g., "1 hour", "1 day", "5 minutes")
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
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
async fn get_today_stats(
    State(service): State<Arc<ProxyLogService>>,
    Query(query): Query<StatsQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
async fn get_time_bucket_stats(
    State(service): State<Arc<ProxyLogService>>,
    Query(query): Query<TimeBucketStatsQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
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
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

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

/// Get health summaries for multiple projects (last 1 hour)
#[utoipa::path(
    get,
    path = "/proxy-logs/stats/projects-health",
    params(ProjectsHealthQuery),
    responses(
        (status = 200, description = "Health summaries per project", body = ProjectsHealthResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
async fn get_projects_health(
    State(service): State<Arc<ProxyLogService>>,
    Query(query): Query<ProjectsHealthQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let project_ids: Vec<i32> = query
        .project_ids
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    if project_ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "project_ids must contain at least one valid ID".to_string(),
        ));
    }

    if project_ids.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Maximum 100 project IDs allowed".to_string(),
        ));
    }

    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::hours(1));

    if start_time >= end_time {
        return Err((
            StatusCode::BAD_REQUEST,
            "start_time must be before end_time".to_string(),
        ));
    }

    let summaries = service
        .get_projects_health_summary(&project_ids, start_time, end_time, query.is_bot)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
async fn get_ai_agent_breakdown(
    State(service): State<Arc<ProxyLogService>>,
    Query(query): Query<AiAgentBreakdownQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err((
            StatusCode::BAD_REQUEST,
            "start_time must be before end_time".to_string(),
        ));
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
async fn get_ai_page_breakdown(
    State(service): State<Arc<ProxyLogService>>,
    Query(query): Query<AiAgentBreakdownQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err((
            StatusCode::BAD_REQUEST,
            "start_time must be before end_time".to_string(),
        ));
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
async fn get_ai_agent_timeline(
    State(service): State<Arc<ProxyLogService>>,
    Query(query): Query<AiAgentTimelineQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err((
            StatusCode::BAD_REQUEST,
            "start_time must be before end_time".to_string(),
        ));
    }

    let group_by = match query.group_by.as_deref() {
        Some("agent") => AiTimelineGroupBy::Agent,
        Some("provider") | None => AiTimelineGroupBy::Provider,
        Some(other) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("group_by must be 'provider' or 'agent', got '{}'", other),
            ));
        }
    };

    // Validate the optional bucket override here so a bad value is a 400 (like
    // `group_by` above), not a 500 from the service layer — and so the raw user
    // string is never reflected back in the error body. The service re-checks
    // via `is_valid_interval` as defense-in-depth.
    if let Some(ref b) = query.bucket {
        if !ProxyLogService::is_valid_interval(b) {
            return Err((
                StatusCode::BAD_REQUEST,
                "bucket must be a valid interval, e.g. '1 hour', '6 hours' or '1 day'".to_string(),
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Proxy Logs"
)]
async fn get_ai_status_breakdown(
    State(service): State<Arc<ProxyLogService>>,
    Query(query): Query<AiAgentBreakdownQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let end_time = query.end_time.unwrap_or_else(chrono::Utc::now);
    let start_time = query
        .start_time
        .unwrap_or_else(|| end_time - chrono::Duration::days(7));

    if start_time >= end_time {
        return Err((
            StatusCode::BAD_REQUEST,
            "start_time must be before end_time".to_string(),
        ));
    }

    let items = service
        .get_ai_status_breakdown(query.project_id, query.environment_id, start_time, end_time)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        (status = 200, description = "Known AI agents", body = KnownAiAgentsResponse)
    ),
    tag = "Proxy Logs"
)]
async fn list_known_ai_agents() -> impl IntoResponse {
    let items: Vec<AiAgentDescriptor> = crate::ai_agent_detector::known_agents()
        .iter()
        .map(|(_, m)| AiAgentDescriptor {
            provider: m.provider.to_string(),
            agent: m.agent.to_string(),
            purpose: m.purpose.as_str().to_string(),
        })
        .collect();
    Json(KnownAiAgentsResponse { items })
}

/// Create router for proxy log handlers
pub fn create_routes() -> axum::Router<Arc<ProxyLogService>> {
    use axum::routing::get;

    axum::Router::new()
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
        .route("/proxy-logs/ai-agents/known", get(list_known_ai_agents))
}

/// Get OpenAPI documentation for proxy logs handlers
pub fn openapi() -> utoipa::openapi::OpenApi {
    use utoipa::OpenApi;

    #[derive(OpenApi)]
    #[openapi(
        paths(
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
            list_known_ai_agents,
        ),
        components(schemas(
            ProxyLogResponse,
            ProxyLogsPaginatedResponse,
            TodayStatsResponse,
            TimeBucketStatsResponse,
            TimeBucketStats,
            StatsFilters,
            ProjectHealthSummary,
            ProjectsHealthResponse,
            AiAgentBreakdownResponse,
            AiAgentBreakdownRow,
            AiAgentTimelineResponse,
            AiAgentTimelineRow,
            AiPageBreakdownResponse,
            AiPageBreakdownRow,
            AiStatusBreakdownResponse,
            AiStatusBreakdownRow,
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
}
