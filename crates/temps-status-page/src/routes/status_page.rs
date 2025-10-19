use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use temps_core::error_builder::{bad_request, internal_server_error, not_found};
use temps_core::problemdetails::Problem;
use temps_core::DateTime;
use utoipa::OpenApi;

use crate::services::{
    CreateIncidentRequest, CreateMonitorRequest, CurrentStatusResponse, IncidentBucketedResponse,
    IncidentResponse, IncidentUpdateResponse, MonitorResponse, StatusBucketedResponse,
    StatusPageError, StatusPageOverview, StatusPageService, UpdateIncidentStatusRequest,
    UptimeHistoryResponse,
};

/// Application state trait for status page routes
pub trait StatusPageAppState: Send + Sync + 'static {
    fn status_page_service(&self) -> &StatusPageService;
}

/// OpenAPI documentation for status page endpoints
#[derive(OpenApi)]
#[openapi(
    paths(
        get_status_overview,
        create_monitor,
        list_monitors,
        get_monitor,
        delete_monitor,
        get_current_monitor_status,
        get_uptime_history,
        get_bucketed_status,
        create_incident,
        list_incidents,
        get_incident,
        update_incident_status,
        get_incident_updates,
        get_bucketed_incidents,
    ),
    components(
        schemas(
            StatusPageOverview,
            MonitorResponse,
            CreateMonitorRequest,
            CurrentStatusResponse,
            UptimeHistoryResponse,
            StatusBucketedResponse,
            IncidentResponse,
            CreateIncidentRequest,
            UpdateIncidentStatusRequest,
            IncidentUpdateResponse,
            IncidentBucketedResponse,
        )
    ),
    tags(
        (name = "Status Page", description = "Status page and monitoring endpoints")
    )
)]
pub struct StatusPageApiDoc;

#[derive(Deserialize)]
pub struct PaginationQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[derive(Deserialize)]
pub struct IncidentListQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
    pub environment_id: Option<i32>,
    pub status: Option<String>,
}

#[derive(Deserialize)]
pub struct MonitorListQuery {
    pub environment_id: Option<i32>,
}

#[derive(Deserialize)]
pub struct UptimeQuery {
    pub days: Option<i32>,
    pub start_time: DateTime, // ISO 8601 datetime
    pub end_time: DateTime,   // ISO 8601 datetime
}

#[derive(Deserialize)]
pub struct CurrentStatusQuery {
    pub start_time: DateTime, // Custom start time (ISO 8601)
    pub end_time: DateTime,   // Custom end time (ISO 8601)
}

#[derive(Deserialize)]
pub struct BucketedQuery {
    pub interval: Option<String>, // "5min", "hourly", or "daily"
    pub start_time: DateTime,     // ISO 8601 datetime
    pub end_time: DateTime,       // ISO 8601 datetime
}

/// Get status page overview
#[utoipa::path(
    get,
    path = "/projects/{project_id}/status",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved status overview", body = StatusPageOverview),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn get_status_overview<T>(
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Query(query): Query<MonitorListQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .get_status_overview(project_id, query.environment_id)
        .await
        .map(Json)
        .map_err(map_error)
}

/// Create a new monitor
#[utoipa::path(
    post,
    path = "/projects/{project_id}/monitors",
    request_body = CreateMonitorRequest,
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 201, description = "Monitor created successfully", body = MonitorResponse),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn create_monitor<T>(
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateMonitorRequest>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .monitor_service()
        .create_monitor(project_id, request)
        .await
        .map(|monitor| (StatusCode::CREATED, Json(monitor)))
        .map_err(map_error)
}

/// List monitors for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/monitors",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved monitors", body = Vec<MonitorResponse>),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn list_monitors<T>(
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Query(query): Query<MonitorListQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .monitor_service()
        .list_monitors(project_id, query.environment_id)
        .await
        .map(Json)
        .map_err(map_error)
}

/// Get a monitor by ID
#[utoipa::path(
    get,
    path = "/monitors/{monitor_id}",
    params(
        ("monitor_id" = i32, Path, description = "Monitor ID"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved monitor", body = MonitorResponse),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn get_monitor<T>(
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .monitor_service()
        .get_monitor(monitor_id)
        .await
        .map(Json)
        .map_err(map_error)
}

/// Delete a monitor
#[utoipa::path(
    delete,
    path = "/monitors/{monitor_id}",
    params(
        ("monitor_id" = i32, Path, description = "Monitor ID"),
    ),
    responses(
        (status = 204, description = "Monitor deleted successfully"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn delete_monitor<T>(
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .monitor_service()
        .delete_monitor(monitor_id)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(map_error)
}

/// Get current status and uptime metrics for a monitor
#[utoipa::path(
    get,
    path = "/monitors/{monitor_id}/current-status",
    params(
        ("monitor_id" = i32, Path, description = "Monitor ID"),
        ("start_time" = Option<String>, Query, description = "Custom start time (ISO 8601) - overrides timeframe"),
        ("end_time" = Option<String>, Query, description = "Custom end time (ISO 8601)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved current status", body = CurrentStatusResponse),
        (status = 400, description = "Invalid time parameters"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn get_current_monitor_status<T>(
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
    Query(query): Query<CurrentStatusQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    // Use custom timeframe method if any non-default values specified
    app_state
        .status_page_service()
        .monitor_service()
        .get_current_status_with_timeframes(
            monitor_id,
            query.start_time.into(),
            query.end_time.into(),
        )
        .await
        .map(Json)
        .map_err(map_error)
}

/// Get uptime history for a monitor
#[utoipa::path(
    get,
    path = "/monitors/{monitor_id}/uptime",
    params(
        ("monitor_id" = i32, Path, description = "Monitor ID"),
        ("days" = Option<i32>, Query, description = "Number of days of history (default: 60) - ignored if start_time/end_time provided"),
        ("start_time" = String, Query, description = "Start time (ISO 8601) - overrides days parameter"),
        ("end_time" = String, Query, description = "End time (ISO 8601) - defaults to now"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved uptime history", body = UptimeHistoryResponse),
        (status = 400, description = "Invalid time parameters"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn get_uptime_history<T>(
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
    Query(query): Query<UptimeQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .monitor_service()
        .get_uptime_history_range(monitor_id, query.start_time.into(), query.end_time.into())
        .await
        .map(Json)
        .map_err(map_error)
}

/// Get bucketed status data for a monitor using TimescaleDB
#[utoipa::path(
    get,
    path = "/monitors/{monitor_id}/bucketed",
    params(
        ("monitor_id" = i32, Path, description = "Monitor ID"),
        ("interval" = Option<String>, Query, description = "Bucket interval: '5min', 'hourly', or 'daily' (default: hourly)"),
        ("start_time" = Option<String>, Query, description = "Start time (ISO 8601) (default: 24 hours ago)"),
        ("end_time" = Option<String>, Query, description = "End time (ISO 8601) (default: now)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved bucketed status data", body = StatusBucketedResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn get_bucketed_status<T>(
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
    Query(query): Query<BucketedQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    let interval = query.interval.as_deref().unwrap_or("hourly");
    app_state
        .status_page_service()
        .monitor_service()
        .get_bucketed_status(
            monitor_id,
            interval,
            query.start_time.into(),
            query.end_time.into(),
        )
        .await
        .map(Json)
        .map_err(map_error)
}

/// Create a new incident
#[utoipa::path(
    post,
    path = "/projects/{project_id}/incidents",
    request_body = CreateIncidentRequest,
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 201, description = "Incident created successfully", body = IncidentResponse),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn create_incident<T>(
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateIncidentRequest>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .incident_service()
        .create_incident(project_id, request)
        .await
        .map(|incident| (StatusCode::CREATED, Json(incident)))
        .map_err(map_error)
}

/// List incidents for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/incidents",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("status" = Option<String>, Query, description = "Filter by status"),
        ("page" = Option<u64>, Query, description = "Page number"),
        ("page_size" = Option<u64>, Query, description = "Items per page"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved incidents"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn list_incidents<T>(
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Query(query): Query<IncidentListQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    let (incidents, total) = app_state
        .status_page_service()
        .incident_service()
        .list_incidents(
            project_id,
            query.environment_id,
            query.status,
            query.page,
            query.page_size,
        )
        .await
        .map_err(map_error)?;

    Ok(Json(serde_json::json!({
        "incidents": incidents,
        "total": total,
    })))
}

/// Get an incident by ID
#[utoipa::path(
    get,
    path = "/incidents/{incident_id}",
    params(
        ("incident_id" = i32, Path, description = "Incident ID"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved incident", body = IncidentResponse),
        (status = 404, description = "Incident not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn get_incident<T>(
    State(app_state): State<Arc<T>>,
    Path(incident_id): Path<i32>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .incident_service()
        .get_incident(incident_id)
        .await
        .map(Json)
        .map_err(map_error)
}

/// Update incident status
#[utoipa::path(
    patch,
    path = "/incidents/{incident_id}/status",
    request_body = UpdateIncidentStatusRequest,
    params(
        ("incident_id" = i32, Path, description = "Incident ID"),
    ),
    responses(
        (status = 200, description = "Incident status updated successfully", body = IncidentResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Incident not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn update_incident_status<T>(
    State(app_state): State<Arc<T>>,
    Path(incident_id): Path<i32>,
    Json(request): Json<UpdateIncidentStatusRequest>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .incident_service()
        .update_incident_status(incident_id, request)
        .await
        .map(Json)
        .map_err(map_error)
}

/// Get incident updates
#[utoipa::path(
    get,
    path = "/incidents/{incident_id}/updates",
    params(
        ("incident_id" = i32, Path, description = "Incident ID"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved incident updates", body = Vec<IncidentUpdateResponse>),
        (status = 404, description = "Incident not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn get_incident_updates<T>(
    State(app_state): State<Arc<T>>,
    Path(incident_id): Path<i32>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    app_state
        .status_page_service()
        .incident_service()
        .get_incident_updates(incident_id)
        .await
        .map(Json)
        .map_err(map_error)
}

/// Get bucketed incident data for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/incidents/bucketed",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("interval" = Option<String>, Query, description = "Bucket interval: '5min', 'hourly', or 'daily' (default: hourly)"),
        ("start_time" = Option<String>, Query, description = "Start time (ISO 8601) (default: 7 days ago)"),
        ("end_time" = Option<String>, Query, description = "End time (ISO 8601) (default: now)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved bucketed incident data", body = IncidentBucketedResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page"
)]
pub async fn get_bucketed_incidents<T>(
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Query(query): Query<IncidentListQuery>,
    Query(bucket_query): Query<BucketedQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    let interval = bucket_query.interval.as_deref().unwrap_or("hourly");

    app_state
        .status_page_service()
        .incident_service()
        .get_bucketed_incidents(
            project_id,
            query.environment_id,
            interval,
            bucket_query.start_time.into(),
            bucket_query.end_time.into(),
        )
        .await
        .map(Json)
        .map_err(map_error)
}

/// Create router for status page endpoints
pub fn create_router<T>() -> Router<Arc<T>>
where
    T: StatusPageAppState,
{
    Router::new()
        .route("/projects/{project_id}/status", get(get_status_overview))
        .route("/projects/{project_id}/monitors", post(create_monitor))
        .route("/projects/{project_id}/monitors", get(list_monitors))
        .route("/monitors/{monitor_id}", get(get_monitor))
        .route("/monitors/{monitor_id}", delete(delete_monitor))
        .route(
            "/monitors/{monitor_id}/current-status",
            get(get_current_monitor_status),
        )
        .route("/monitors/{monitor_id}/uptime", get(get_uptime_history))
        .route("/monitors/{monitor_id}/bucketed", get(get_bucketed_status))
        .route("/projects/{project_id}/incidents", post(create_incident))
        .route("/projects/{project_id}/incidents", get(list_incidents))
        .route(
            "/projects/{project_id}/incidents/bucketed",
            get(get_bucketed_incidents),
        )
        .route("/incidents/{incident_id}", get(get_incident))
        .route(
            "/incidents/{incident_id}/status",
            patch(update_incident_status),
        )
        .route(
            "/incidents/{incident_id}/updates",
            get(get_incident_updates),
        )
}

fn map_error(error: StatusPageError) -> Problem {
    match error {
        StatusPageError::NotFound => not_found().detail("Resource not found").build(),
        StatusPageError::Validation(msg) => bad_request().detail(&msg).build(),
        StatusPageError::InvalidRequest(msg) => bad_request().detail(&msg).build(),
        StatusPageError::Database(err) => {
            tracing::error!("Database error: {}", err);
            internal_server_error()
                .detail("Database error while processing status page request")
                .build()
        }
        StatusPageError::Internal(msg) => {
            tracing::error!("Internal error: {}", msg);
            internal_server_error().detail(&msg).build()
        }
    }
}
