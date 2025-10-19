use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::{error_builder::ErrorBuilder, problemdetails::Problem};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use super::types::AppState;
use crate::service::request_log_service::RequestLogResponse;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_request_logs,
        get_request_log_by_id,
    ),
    components(
        schemas(
            RequestLogsQuery,
            RequestLogsResponse,
            RequestLogResponse,
        )
    ),
    info(
        title = "Request Logs API",
        description = "API endpoints for querying request logs from the proxy. \
        Allows filtering by project, environment, and deployment. \
        If project_id is not provided, returns all system logs.",
        version = "1.0.0"
    ),
    tags(
        (name = "Request Logs", description = "Request log query endpoints")
    )
)]
pub struct RequestLogsApiDoc;

#[derive(Debug, Deserialize, ToSchema)]
pub struct RequestLogsQuery {
    /// Project ID (optional - if not provided, returns all system logs)
    pub project_id: Option<i32>,
    /// Environment ID (optional)
    pub environment_id: Option<i32>,
    /// Deployment ID (optional)
    pub deployment_id: Option<i32>,
    /// HTTP status code filter (optional)
    pub status_code: Option<i32>,
    /// HTTP method filter (optional, e.g., GET, POST, PUT, DELETE)
    pub method: Option<String>,
    /// Start date filter (milliseconds since epoch)
    pub start_date: Option<i64>,
    /// End date filter (milliseconds since epoch)
    pub end_date: Option<i64>,
    /// Page number (default: 1)
    pub page: Option<u64>,
    /// Page size/limit (default: 20, max: 100)
    pub limit: Option<u64>,
    /// Offset for pagination (auto-calculated from page if not provided)
    pub offset: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RequestLogsResponse {
    pub logs: Vec<RequestLogResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[utoipa::path(
    tag = "Request Logs",
    get,
    path = "request-logs",
    params(
        ("project_id" = Option<i32>, Query, description = "Project ID (optional - if not provided, returns all system logs)"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("deployment_id" = Option<i32>, Query, description = "Deployment ID (optional)"),
        ("status_code" = Option<i32>, Query, description = "HTTP status code filter (optional)"),
        ("method" = Option<String>, Query, description = "HTTP method filter (optional)"),
        ("start_date" = Option<i64>, Query, description = "Start date filter in milliseconds since epoch (optional)"),
        ("end_date" = Option<i64>, Query, description = "End date filter in milliseconds since epoch (optional)"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("limit" = Option<u64>, Query, description = "Page size/limit (default: 20, max: 100)"),
        ("offset" = Option<u64>, Query, description = "Offset for pagination (auto-calculated from page if not provided)"),
    ),
    responses(
        (status = 200, description = "List of request logs", body = RequestLogsResponse),
        (status = 400, description = "Invalid query parameters"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_request_logs(
    State(app_state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Query(query): Query<RequestLogsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    let page = query.page.unwrap_or(1);
    let limit = std::cmp::min(query.limit.unwrap_or(20), 100);
    let offset = query.offset.unwrap_or((page - 1) * limit);

    match app_state
        .request_log_service
        .get_logs(
            query.project_id,
            query.environment_id,
            query.deployment_id,
            query.status_code,
            query.method.as_deref(),
            query.start_date,
            query.end_date,
            limit,
            offset,
        )
        .await
    {
        Ok((logs, total)) => Ok((
            StatusCode::OK,
            Json(RequestLogsResponse {
                logs,
                total,
                page,
                page_size: limit,
            }),
        )
            .into_response()),
        Err(e) => {
            error!("Error fetching request logs: {:?}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to fetch request logs")
                .detail(&format!("Error fetching request logs: {}", e))
                .build())
        }
    }
}

#[utoipa::path(
    tag = "Request Logs",
    get,
    path = "request-logs/{id}",
    params(
        ("id" = i32, Path, description = "Request log ID"),
        ("project_id" = Option<i32>, Query, description = "Project ID (optional - if not provided, searches all system logs)"),
    ),
    responses(
        (status = 200, description = "Request log found", body = RequestLogResponse),
        (status = 404, description = "Request log not found"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn get_request_log_by_id(
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Query(query): Query<RequestLogsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, LoadBalancerRead);

    match app_state
        .request_log_service
        .get_log_by_id(id, query.project_id)
        .await
    {
        Ok(Some(log)) => Ok((StatusCode::OK, Json(log)).into_response()),
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Request log not found")
            .detail(&format!("No request log found with ID {}", id))
            .build()),
        Err(e) => {
            error!("Error fetching request log: {:?}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to fetch request log")
                .detail(&format!("Error fetching request log: {}", e))
                .build())
        }
    }
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/request-logs", get(get_request_logs))
        .route("/request-logs/{id}", get(get_request_log_by_id))
}
