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
    ProxyLogResponse, ProxyLogService, StatsFilters, TimeBucketStats, TodayStatsResponse,
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
            routing_status: query.routing_status,
            request_source: query.request_source,
            is_bot: query.is_bot,
            device_type: query.device_type,
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
    /// Filter by routing status
    pub routing_status: Option<String>,
    /// Filter by request source
    pub request_source: Option<String>,
    /// Filter by bot detection
    pub is_bot: Option<bool>,
    /// Filter by device type
    pub device_type: Option<String>,
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
        || query.routing_status.is_some()
        || query.request_source.is_some()
        || query.is_bot.is_some()
        || query.device_type.is_some()
    {
        Some(StatsFilters {
            method: query.method,
            client_ip: query.client_ip,
            project_id: query.project_id,
            environment_id: query.environment_id,
            deployment_id: query.deployment_id,
            host: query.host,
            status_code: query.status_code,
            routing_status: query.routing_status,
            request_source: query.request_source,
            is_bot: query.is_bot,
            device_type: query.device_type,
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
        ),
        components(schemas(
            ProxyLogResponse,
            ProxyLogsPaginatedResponse,
            TodayStatsResponse,
            TimeBucketStatsResponse,
            TimeBucketStats,
            StatsFilters,
        ))
    )]
    struct ApiDoc;

    ApiDoc::openapi()
}
