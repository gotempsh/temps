use super::types::AppState;
use crate::services::{ErrorEventDomain, ErrorGroupDomain, ErrorTrackingError};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::DateTime;
use utoipa::{IntoParams, OpenApi, ToSchema};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_error_groups,
        get_error_group,
        update_error_group,
        list_error_events,
        get_error_event,
        get_error_stats,
        get_error_dashboard_stats,
        get_error_time_series,
        has_error_groups,
    ),
    components(schemas(
        ErrorGroupResponse,
        ErrorEventResponse,
        ErrorGroupStatsResponse,
        ErrorDashboardStatsResponse,
        ErrorTimeSeriesDataResponse,
        ListErrorGroupsQuery,
        ListErrorEventsQuery,
        UpdateErrorGroupRequest,
        ErrorDashboardStatsQuery,
        ErrorTimeSeriesQuery,
        HasErrorGroupsResponse,
        PaginatedErrorGroupsResponse,
        PaginatedErrorEventsResponse,
        PaginationMeta,
    )),
    tags(
        (name = "error-tracking", description = "Error tracking data fetching endpoints")
    )
)]
pub struct ErrorTrackingApiDoc;

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-groups",
            get(list_error_groups),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}",
            get(get_error_group).put(update_error_group),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/events",
            get(list_error_events),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/events/{event_id}",
            get(get_error_event),
        )
        .route("/projects/{project_id}/error-stats", get(get_error_stats))
        .route(
            "/projects/{project_id}/error-dashboard-stats",
            get(get_error_dashboard_stats),
        )
        .route(
            "/projects/{project_id}/error-time-series",
            get(get_error_time_series),
        )
        .route(
            "/projects/{project_id}/has-error-groups",
            get(has_error_groups),
        )
}

// ===== Request/Response Types =====

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ListErrorGroupsQuery {
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
    pub status: Option<String>,
    pub environment_id: Option<i32>,
    pub start_date: Option<DateTime>,
    pub end_date: Option<DateTime>,
    pub sort_by: Option<String>,
    #[serde(default = "default_sort_order")]
    pub sort_order: String,
}

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ListErrorEventsQuery {
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateErrorGroupRequest {
    pub status: String,
    pub assigned_to: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ErrorTimeSeriesQuery {
    pub start_time: DateTime,
    pub end_time: DateTime,
    /// Time bucket size (e.g., "1h", "15m", "1d", "1 hour", "30 minutes")
    #[serde(default = "default_interval")]
    #[schema(example = "1h")]
    pub bucket: String,
}

#[derive(Debug, Deserialize, ToSchema, IntoParams)]
pub struct ErrorDashboardStatsQuery {
    pub start_time: DateTime,
    pub end_time: DateTime,
    pub environment_id: Option<i32>,
    pub compare_to_previous: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupStatsResponse {
    pub total_groups: i64,
    pub unresolved_groups: i64,
    pub resolved_groups: i64,
    pub ignored_groups: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorTimeSeriesDataResponse {
    pub timestamp: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDashboardStatsResponse {
    pub total_errors: i64,
    pub total_errors_previous_period: i64,
    pub total_errors_change_percent: f64,
    pub error_groups: i64,
    pub error_groups_previous_period: i64,
    pub start_time: DateTime,
    pub end_time: DateTime,
    pub comparison_start_time: Option<DateTime>,
    pub comparison_end_time: Option<DateTime>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorGroupResponse {
    pub id: i32,
    pub title: String,
    pub error_type: String,
    pub message_template: Option<String>,
    pub first_seen: String,
    pub last_seen: String,
    pub total_count: i32,
    pub status: String,
    pub assigned_to: Option<String>,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorEventResponse {
    pub id: i64,
    pub error_group_id: i32,
    pub timestamp: String,
    pub created_at: String,
    /// Source of the error event (e.g., "sentry", "custom", "bugsnag")
    pub source: Option<String>,
    /// Full error event data (contains raw Sentry event or custom error data)
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HasErrorGroupsResponse {
    pub has_error_groups: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedErrorGroupsResponse {
    pub data: Vec<ErrorGroupResponse>,
    pub pagination: PaginationMeta,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginationMeta {
    pub page: u64,
    pub page_size: u64,
    pub total_count: u64,
    pub total_pages: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedErrorEventsResponse {
    pub data: Vec<ErrorEventResponse>,
    pub pagination: PaginationMeta,
}

fn default_page() -> u64 {
    1
}
fn default_page_size() -> u64 {
    20
}
fn default_sort_order() -> String {
    "desc".to_string()
}
fn default_interval() -> String {
    "1h".to_string()
}

// ===== Conversions =====

impl From<ErrorGroupDomain> for ErrorGroupResponse {
    fn from(group: ErrorGroupDomain) -> Self {
        Self {
            id: group.id,
            title: group.title,
            error_type: group.error_type,
            message_template: group.message_template,
            first_seen: group.first_seen.to_rfc3339(),
            last_seen: group.last_seen.to_rfc3339(),
            total_count: group.total_count,
            status: group.status,
            assigned_to: group.assigned_to,
            project_id: group.project_id,
            environment_id: group.environment_id,
            deployment_id: group.deployment_id,
            visitor_id: group.visitor_id,
            created_at: group.created_at.to_rfc3339(),
            updated_at: group.updated_at.to_rfc3339(),
        }
    }
}

impl From<ErrorEventDomain> for ErrorEventResponse {
    fn from(event: ErrorEventDomain) -> Self {
        Self {
            id: event.id,
            error_group_id: event.error_group_id,
            timestamp: event.timestamp.to_rfc3339(),
            created_at: event.created_at.to_rfc3339(),
            source: event.source,
            data: event.data,
        }
    }
}

// ===== Error Handling =====

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
    pub details: Option<String>,
}

impl axum::response::IntoResponse for ErrorTrackingError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            ErrorTrackingError::Database(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            ),
            ErrorTrackingError::GroupNotFound => {
                (StatusCode::NOT_FOUND, "Error group not found".to_string())
            }
            ErrorTrackingError::EventNotFound => {
                (StatusCode::NOT_FOUND, "Error event not found".to_string())
            }
            ErrorTrackingError::InvalidFingerprint => {
                (StatusCode::BAD_REQUEST, "Invalid fingerprint".to_string())
            }
            ErrorTrackingError::EmbeddingService(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Embedding service error: {}", msg),
            ),
            ErrorTrackingError::Validation(msg) => (
                StatusCode::BAD_REQUEST,
                format!("Validation error: {}", msg),
            ),
            ErrorTrackingError::ProjectNotFound => {
                (StatusCode::NOT_FOUND, "Project not found".to_string())
            }
        };

        (
            status,
            Json(ErrorResponse {
                error: message,
                details: None,
            }),
        )
            .into_response()
    }
}

// ===== Handlers =====

/// List error groups for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups",
    responses(
        (status = 200, description = "Paginated list of error groups", body = PaginatedErrorGroupsResponse),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ListErrorGroupsQuery
    ),
    tag = "error-tracking"
)]
pub async fn list_error_groups(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListErrorGroupsQuery>,
) -> Result<Json<PaginatedErrorGroupsResponse>, ErrorTrackingError> {
    let page = query.page;
    let page_size = std::cmp::min(query.page_size, 100);

    let (groups, total_count) = state
        .error_tracking_service
        .list_error_groups(
            project_id,
            Some(page),
            Some(page_size),
            query.status,
            query.environment_id,
            query.sort_by,
            Some(query.sort_order),
        )
        .await?;

    let total_pages = if total_count > 0 {
        (total_count as f64 / page_size as f64).ceil() as u64
    } else {
        0
    };

    Ok(Json(PaginatedErrorGroupsResponse {
        data: groups.into_iter().map(ErrorGroupResponse::from).collect(),
        pagination: PaginationMeta {
            page,
            page_size,
            total_count,
            total_pages,
        },
    }))
}

/// Get a specific error group
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}",
    responses(
        (status = 200, description = "Error group details", body = ErrorGroupResponse),
        (status = 404, description = "Error group not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking"
)]
pub async fn get_error_group(
    State(state): State<Arc<AppState>>,
    Path((project_id, group_id)): Path<(i32, i32)>,
) -> Result<Json<ErrorGroupResponse>, ErrorTrackingError> {
    let group = state
        .error_tracking_service
        .get_error_group(group_id, project_id)
        .await?;

    Ok(Json(ErrorGroupResponse::from(group)))
}

/// Update error group status
#[utoipa::path(
    put,
    path = "/projects/{project_id}/error-groups/{group_id}",
    request_body = UpdateErrorGroupRequest,
    responses(
        (status = 200, description = "Error group updated successfully"),
        (status = 404, description = "Error group not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking"
)]
pub async fn update_error_group(
    State(state): State<Arc<AppState>>,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateErrorGroupRequest>,
) -> Result<StatusCode, ErrorTrackingError> {
    state
        .error_tracking_service
        .update_error_group_status(group_id, project_id, request.status, request.assigned_to)
        .await?;

    Ok(StatusCode::OK)
}

/// List error events for a specific group
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/events",
    responses(
        (status = 200, description = "Paginated list of error events", body = PaginatedErrorEventsResponse),
        (status = 404, description = "Error group not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ListErrorEventsQuery
    ),
    tag = "error-tracking"
)]
pub async fn list_error_events(
    State(state): State<Arc<AppState>>,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Query(query): Query<ListErrorEventsQuery>,
) -> Result<Json<PaginatedErrorEventsResponse>, ErrorTrackingError> {
    let page = query.page;
    let page_size = std::cmp::min(query.page_size, 100);

    let (events, total_count) = state
        .error_tracking_service
        .list_error_events(group_id, project_id, Some(page), Some(page_size))
        .await?;

    let total_pages = if total_count > 0 {
        (total_count as f64 / page_size as f64).ceil() as u64
    } else {
        0
    };

    Ok(Json(PaginatedErrorEventsResponse {
        data: events.into_iter().map(ErrorEventResponse::from).collect(),
        pagination: PaginationMeta {
            page,
            page_size,
            total_count,
            total_pages,
        },
    }))
}

/// Get a specific error event
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/events/{event_id}",
    responses(
        (status = 200, description = "Error event details", body = ErrorEventResponse),
        (status = 404, description = "Event not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ("event_id" = i64, Path, description = "Error event ID")
    ),
    tag = "error-tracking"
)]
pub async fn get_error_event(
    State(state): State<Arc<AppState>>,
    Path((project_id, group_id, event_id)): Path<(i32, i32, i64)>,
) -> Result<Json<ErrorEventResponse>, ErrorTrackingError> {
    let event = state
        .error_tracking_service
        .get_error_event(event_id, group_id, project_id)
        .await?;

    Ok(Json(ErrorEventResponse::from(event)))
}

/// Get error statistics for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-stats",
    responses(
        (status = 200, description = "Error statistics", body = ErrorGroupStatsResponse),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "error-tracking"
)]
pub async fn get_error_stats(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<ErrorGroupStatsResponse>, ErrorTrackingError> {
    let stats = state
        .error_tracking_service
        .get_error_stats(project_id, None)
        .await?;

    Ok(Json(ErrorGroupStatsResponse {
        total_groups: stats.total_groups,
        unresolved_groups: stats.unresolved_groups,
        resolved_groups: stats.resolved_groups,
        ignored_groups: stats.ignored_groups,
    }))
}

/// Get error dashboard statistics
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-dashboard-stats",
    responses(
        (status = 200, description = "Error dashboard statistics", body = ErrorDashboardStatsResponse),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ErrorDashboardStatsQuery
    ),
    tag = "error-tracking"
)]
pub async fn get_error_dashboard_stats(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ErrorDashboardStatsQuery>,
) -> Result<Json<ErrorDashboardStatsResponse>, ErrorTrackingError> {
    let stats = state
        .error_tracking_service
        .get_dashboard_stats(
            project_id,
            query.start_time.into(),
            query.end_time.into(),
            query.environment_id,
            query.compare_to_previous.unwrap_or(true),
        )
        .await?;

    Ok(Json(ErrorDashboardStatsResponse {
        total_errors: stats.total_errors,
        total_errors_previous_period: stats.total_errors_previous_period,
        total_errors_change_percent: stats.total_errors_change_percent,
        error_groups: stats.error_groups,
        error_groups_previous_period: stats.error_groups_previous_period,
        start_time: query.start_time,
        end_time: query.end_time,
        comparison_start_time: stats.comparison_start_time.map(|dt| dt.into()),
        comparison_end_time: stats.comparison_end_time.map(|dt| dt.into()),
    }))
}

/// Get error time series data for charts
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-time-series",
    responses(
        (status = 200, description = "Error time series data", body = Vec<ErrorTimeSeriesDataResponse>),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ErrorTimeSeriesQuery
    ),
    tag = "error-tracking"
)]
pub async fn get_error_time_series(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ErrorTimeSeriesQuery>,
) -> Result<Json<Vec<ErrorTimeSeriesDataResponse>>, ErrorTrackingError> {
    let data = state
        .error_tracking_service
        .get_error_time_series(
            project_id,
            query.start_time.into(),
            query.end_time.into(),
            &query.bucket,
        )
        .await?;

    let response: Vec<ErrorTimeSeriesDataResponse> = data
        .into_iter()
        .map(|item| ErrorTimeSeriesDataResponse {
            timestamp: item.timestamp.to_rfc3339(),
            count: item.count,
        })
        .collect();

    Ok(Json(response))
}

/// Check if project has any error groups
#[utoipa::path(
    get,
    path = "/projects/{project_id}/has-error-groups",
    responses(
        (status = 200, description = "Error groups existence check", body = HasErrorGroupsResponse),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "error-tracking"
)]
pub async fn has_error_groups(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<HasErrorGroupsResponse>, ErrorTrackingError> {
    let has_error_groups = state
        .error_tracking_service
        .has_error_groups(project_id)
        .await?;

    Ok(Json(HasErrorGroupsResponse { has_error_groups }))
}
