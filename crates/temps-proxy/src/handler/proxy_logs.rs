use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use temps_core::DateTime;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};

use crate::service::proxy_log_service::{ProxyLogResponse, ProxyLogService};

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

/// Create router for proxy log handlers
pub fn create_routes() -> axum::Router<Arc<ProxyLogService>> {
    use axum::routing::get;

    axum::Router::new()
        .route("/proxy-logs", get(get_proxy_logs))
        .route("/proxy-logs/{id}", get(get_proxy_log_by_id))
        .route("/proxy-logs/request/{request_id}", get(get_proxy_log_by_request_id))
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
        ),
        components(schemas(
            ProxyLogResponse,
            ProxyLogsPaginatedResponse,
        ))
    )]
    struct ApiDoc;

    ApiDoc::openapi()
}
