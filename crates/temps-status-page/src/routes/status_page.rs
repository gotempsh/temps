// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{collections::BTreeSet, sync::Arc};

use chrono::{Duration, Utc};

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use temps_auth::{permission_guard, project_access_guard, project_permission_guard, RequireAuth};
use temps_core::error_builder::{bad_request, forbidden, internal_server_error, not_found};
use temps_core::problemdetails::{PermissionDenialKind, Problem};
use temps_core::{AuditLogger, DateTime, RequestMetadata};
use utoipa::OpenApi;

use super::audit::{StatusPageMutationAction, StatusPageMutationAudit};
use crate::services::{
    CreateIncidentRequest, CreateMonitorRequest, CurrentStatusResponse, IncidentBucketedResponse,
    IncidentResponse, IncidentUpdateResponse, MonitorResponse, ProjectMonitorHealth,
    StatusBucketedResponse, StatusPageError, StatusPageOverview, StatusPageService,
    UpdateIncidentStatusRequest, UptimeHistoryResponse,
};

/// Application state trait for status page routes
pub trait StatusPageAppState: Send + Sync + 'static {
    fn status_page_service(&self) -> &StatusPageService;
    fn telemetry(&self) -> &std::sync::Arc<dyn temps_core::TelemetryReporter>;
    fn audit_service(&self) -> &Arc<dyn AuditLogger>;
    /// Optional checker for team-based project access (human sessions only).
    fn project_access_checker(&self) -> Option<Arc<dyn temps_core::ProjectAccessChecker>>;
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
        get_projects_monitor_health,
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
            ProjectMonitorHealth,
            ProjectsMonitorHealthResponse,
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
    pub start_time: Option<DateTime>, // ISO 8601 datetime -- overrides `days` when set
    pub end_time: Option<DateTime>,   // ISO 8601 datetime -- overrides `days` when set
}

#[derive(Deserialize)]
pub struct CurrentStatusQuery {
    pub start_time: Option<DateTime>, // Custom start time (ISO 8601) -- defaults to last 24h when unset
    pub end_time: Option<DateTime>, // Custom end time (ISO 8601) -- defaults to last 24h when unset
}

#[derive(Deserialize)]
pub struct BucketedQuery {
    pub interval: Option<String>,     // "5min", "hourly", or "daily"
    pub start_time: Option<DateTime>, // ISO 8601 datetime -- defaults to 24 hours ago when unset
    pub end_time: Option<DateTime>,   // ISO 8601 datetime -- defaults to now when unset
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_status_overview<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Query(query): Query<MonitorListQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker());
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn create_monitor<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateMonitorRequest>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    project_permission_guard!(
        auth,
        StatusPageCreate,
        project_id,
        app_state.project_access_checker()
    );
    let monitor = app_state
        .status_page_service()
        .monitor_service()
        .create_monitor(project_id, request)
        .await
        .map_err(map_error)?;

    app_state.telemetry().report(
        temps_core::TelemetryEvent::new(temps_core::TelemetryEventKind::StatusPagePublished)
            .with("monitor_type", monitor.monitor_type.clone()),
    );

    record_status_page_audit(
        app_state.as_ref(),
        StatusPageMutationAudit {
            actor_user_id: auth.user_id_opt(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
            action: StatusPageMutationAction::MonitorCreated,
            project_id,
            resource_type: "monitor",
            resource_id: monitor.id,
            environment_id: monitor.environment_id,
            monitor_id: Some(monitor.id),
            status: None,
        },
    )
    .await;

    Ok((StatusCode::CREATED, Json(monitor)))
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn list_monitors<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Query(query): Query<MonitorListQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker());
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_monitor<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    let project_id = app_state
        .status_page_service()
        .monitor_service()
        .get_monitor_project_id(monitor_id)
        .await
        .map_err(map_error)?;
    project_access_guard!(auth, project_id, app_state.project_access_checker());
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn delete_monitor<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(monitor_id): Path<i32>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    // Reject callers without the coarse permission before resolving the
    // resource's owner, so a forbidden caller cannot distinguish a missing id
    // from an existing monitor in another project.
    permission_guard!(auth, StatusPageDelete);
    let project_id = app_state
        .status_page_service()
        .monitor_service()
        .get_monitor_project_id(monitor_id)
        .await
        .map_err(map_error)?;
    project_permission_guard!(
        auth,
        StatusPageDelete,
        project_id,
        app_state.project_access_checker()
    );
    app_state
        .status_page_service()
        .monitor_service()
        .delete_monitor(monitor_id)
        .await
        .map_err(map_error)?;

    record_status_page_audit(
        app_state.as_ref(),
        StatusPageMutationAudit {
            actor_user_id: auth.user_id_opt(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
            action: StatusPageMutationAction::MonitorDeleted,
            project_id,
            resource_type: "monitor",
            resource_id: monitor_id,
            environment_id: None,
            monitor_id: Some(monitor_id),
            status: None,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_current_monitor_status<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
    Query(query): Query<CurrentStatusQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    let project_id = app_state
        .status_page_service()
        .monitor_service()
        .get_monitor_project_id(monitor_id)
        .await
        .map_err(map_error)?;
    project_access_guard!(auth, project_id, app_state.project_access_checker());
    // Custom timeframe only when BOTH bounds are given; otherwise the documented
    // default (last 24h) applies. These were previously non-optional `DateTime`
    // fields, so every call had to pass both or 400 -- making the 24h-default
    // path (and this doc comment's own premise) unreachable.
    let result = match (query.start_time, query.end_time) {
        (Some(start_time), Some(end_time)) => {
            app_state
                .status_page_service()
                .monitor_service()
                .get_current_status_with_timeframes(monitor_id, *start_time, *end_time)
                .await
        }
        _ => {
            app_state
                .status_page_service()
                .monitor_service()
                .get_current_status(monitor_id)
                .await
        }
    };
    result.map(Json).map_err(map_error)
}

/// Get uptime history for a monitor
#[utoipa::path(
    get,
    path = "/monitors/{monitor_id}/uptime",
    params(
        ("monitor_id" = i32, Path, description = "Monitor ID"),
        ("days" = Option<i32>, Query, description = "Number of days of history (default: 60) - ignored if start_time/end_time provided"),
        ("start_time" = Option<String>, Query, description = "Start time (ISO 8601) - overrides days parameter"),
        ("end_time" = Option<String>, Query, description = "End time (ISO 8601) - defaults to now"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved uptime history", body = UptimeHistoryResponse),
        (status = 400, description = "Invalid time parameters"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_uptime_history<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
    Query(query): Query<UptimeQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    let project_id = app_state
        .status_page_service()
        .monitor_service()
        .get_monitor_project_id(monitor_id)
        .await
        .map_err(map_error)?;
    project_access_guard!(auth, project_id, app_state.project_access_checker());
    // Custom range only when BOTH bounds are given; otherwise fall back to the
    // `days`-based default (see get_uptime_history's own doc: 60 days). These
    // were previously non-optional `DateTime` fields, so `days` could never
    // actually take effect -- every call had to pass an explicit range.
    let result = match (query.start_time, query.end_time) {
        (Some(start_time), Some(end_time)) => {
            app_state
                .status_page_service()
                .monitor_service()
                .get_uptime_history_range(monitor_id, *start_time, *end_time)
                .await
        }
        _ => {
            app_state
                .status_page_service()
                .monitor_service()
                .get_uptime_history(monitor_id, query.days)
                .await
        }
    };
    result.map(Json).map_err(map_error)
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Monitor not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_bucketed_status<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(monitor_id): Path<i32>,
    Query(query): Query<BucketedQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    let project_id = app_state
        .status_page_service()
        .monitor_service()
        .get_monitor_project_id(monitor_id)
        .await
        .map_err(map_error)?;
    project_access_guard!(auth, project_id, app_state.project_access_checker());
    let interval = query.interval.as_deref().unwrap_or("hourly");
    // These were previously non-optional `DateTime` fields, so the documented
    // "(default: 24 hours ago)" / "(default: now)" behavior was unreachable --
    // every call had to pass both explicitly or 400.
    let end_time = query.end_time.map(|d| *d).unwrap_or_else(Utc::now);
    let start_time = query
        .start_time
        .map(|d| *d)
        .unwrap_or_else(|| end_time - Duration::hours(24));
    app_state
        .status_page_service()
        .monitor_service()
        .get_bucketed_status(monitor_id, interval, start_time, end_time)
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn create_incident<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateIncidentRequest>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    project_permission_guard!(
        auth,
        StatusPageCreate,
        project_id,
        app_state.project_access_checker()
    );
    let incident = app_state
        .status_page_service()
        .incident_service()
        .create_incident(project_id, request)
        .await
        .map_err(map_error)?;

    record_status_page_audit(
        app_state.as_ref(),
        StatusPageMutationAudit {
            actor_user_id: auth.user_id_opt(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
            action: StatusPageMutationAction::IncidentCreated,
            project_id,
            resource_type: "incident",
            resource_id: incident.id,
            environment_id: incident.environment_id,
            monitor_id: incident.monitor_id,
            status: Some(incident.status.clone()),
        },
    )
    .await;

    Ok((StatusCode::CREATED, Json(incident)))
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn list_incidents<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Query(query): Query<IncidentListQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker());
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Incident not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_incident<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(incident_id): Path<i32>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    let project_id = app_state
        .status_page_service()
        .incident_service()
        .get_incident_project_id(incident_id)
        .await
        .map_err(map_error)?;
    project_access_guard!(auth, project_id, app_state.project_access_checker());
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Incident not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn update_incident_status<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(incident_id): Path<i32>,
    Json(request): Json<UpdateIncidentStatusRequest>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    // Run the instance-wide ceiling before any ownership lookup. This keeps
    // nonexistent and cross-project ids indistinguishable to forbidden users.
    permission_guard!(auth, StatusPageWrite);
    let project_id = app_state
        .status_page_service()
        .incident_service()
        .get_incident_project_id(incident_id)
        .await
        .map_err(map_error)?;
    project_permission_guard!(
        auth,
        StatusPageWrite,
        project_id,
        app_state.project_access_checker()
    );
    let incident = app_state
        .status_page_service()
        .incident_service()
        .update_incident_status(incident_id, request)
        .await
        .map_err(map_error)?;

    record_status_page_audit(
        app_state.as_ref(),
        StatusPageMutationAudit {
            actor_user_id: auth.user_id_opt(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
            action: StatusPageMutationAction::IncidentStatusUpdated,
            project_id,
            resource_type: "incident",
            resource_id: incident.id,
            environment_id: incident.environment_id,
            monitor_id: incident.monitor_id,
            status: Some(incident.status.clone()),
        },
    )
    .await;

    Ok(Json(incident))
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Incident not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_incident_updates<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(incident_id): Path<i32>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    let project_id = app_state
        .status_page_service()
        .incident_service()
        .get_incident_project_id(incident_id)
        .await
        .map_err(map_error)?;
    project_access_guard!(auth, project_id, app_state.project_access_checker());
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_bucketed_incidents<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Path(project_id): Path<i32>,
    Query(query): Query<IncidentListQuery>,
    Query(bucket_query): Query<BucketedQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker());
    let interval = bucket_query.interval.as_deref().unwrap_or("hourly");
    // Previously non-optional `DateTime` fields made the documented
    // "(default: 7 days ago)" / "(default: now)" behavior unreachable -- every
    // call had to pass both explicitly or 400.
    let end_time = bucket_query.end_time.map(|d| *d).unwrap_or_else(Utc::now);
    let start_time = bucket_query
        .start_time
        .map(|d| *d)
        .unwrap_or_else(|| end_time - Duration::days(7));

    app_state
        .status_page_service()
        .incident_service()
        .get_bucketed_incidents(
            project_id,
            query.environment_id,
            interval,
            start_time,
            end_time,
        )
        .await
        .map(Json)
        .map_err(map_error)
}

/// Query parameters for batch project health
#[derive(Deserialize, utoipa::IntoParams)]
pub struct ProjectsHealthQuery {
    /// Comma-separated list of project IDs
    pub project_ids: String,
}

/// Batch response for projects health
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ProjectsMonitorHealthResponse {
    pub projects: std::collections::HashMap<String, ProjectMonitorHealth>,
}

/// Get monitor-based health summaries for multiple projects in a single query
#[utoipa::path(
    get,
    path = "/monitors-health/projects",
    params(ProjectsHealthQuery),
    responses(
        (status = 200, description = "Health summaries per project", body = ProjectsMonitorHealthResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_projects_monitor_health<T>(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<T>>,
    Query(query): Query<ProjectsHealthQuery>,
) -> Result<impl IntoResponse, Problem>
where
    T: StatusPageAppState,
{
    permission_guard!(auth, StatusPageRead);

    let project_ids: Vec<i32> = query
        .project_ids
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    if project_ids.is_empty() {
        return Err(bad_request()
            .detail("project_ids must contain at least one valid ID")
            .build());
    }

    if project_ids.len() > 100 {
        return Err(bad_request()
            .detail("Maximum 100 project IDs allowed")
            .build());
    }

    require_projects_monitor_health_access(
        &auth,
        app_state.project_access_checker().as_deref(),
        &project_ids,
    )
    .await?;

    let summaries = app_state
        .status_page_service()
        .monitor_service()
        .get_projects_monitor_health(&project_ids)
        .await
        .map_err(map_error)?;

    let projects: std::collections::HashMap<String, ProjectMonitorHealth> = summaries
        .into_iter()
        .map(|s| (s.project_id.to_string(), s))
        .collect();

    Ok(Json(ProjectsMonitorHealthResponse { projects }))
}

/// Authorize a bulk status-page read before any project health is queried.
///
/// The decision is intentionally all-or-nothing: filtering an unauthorized
/// project out of the response would let callers probe project existence from
/// response shape. Team checkers are consulted through their batch APIs so a
/// request with up to 100 project ids does not cause one authorization query
/// sequence per project.
async fn require_projects_monitor_health_access(
    auth: &temps_auth::AuthContext,
    checker: Option<&dyn temps_core::ProjectAccessChecker>,
    project_ids: &[i32],
) -> Result<(), Problem> {
    let unique_project_ids: Vec<i32> = project_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // Deployment tokens carry no user identity for team checks. Keep their
    // existing project boundary explicit on this bulk endpoint: every id must
    // match the single project bound into the token.
    if auth.is_deployment_token() {
        let scoped_project_id = auth.project_id();
        if scoped_project_id.is_some()
            && unique_project_ids
                .iter()
                .all(|project_id| Some(*project_id) == scoped_project_id)
        {
            return Ok(());
        }

        return Err(projects_monitor_health_access_denied(
            PermissionDenialKind::CrossProjectScope,
        ));
    }

    // Instance administrators retain the same bypass as the single-project
    // project access and permission guards.
    if auth.is_admin() || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
        return Ok(());
    }

    let Some(checker) = checker else {
        // No project-access plugin is registered in plain OSS deployments.
        return Ok(());
    };
    let Some(user_id) = auth.user_id_opt() else {
        tracing::error!(
            project_ids = ?unique_project_ids,
            "status-page bulk project authorization had no user principal"
        );
        return Err(projects_monitor_health_access_check_failed());
    };

    let required_permission = temps_auth::permissions::Permission::StatusPageRead.to_string();
    let permissions = checker
        .effective_project_permissions_batch(user_id, &unique_project_ids)
        .await
        .map_err(|error| {
            tracing::error!(
                user_id,
                project_ids = ?unique_project_ids,
                error = %error,
                "status-page bulk project permission check failed"
            );
            projects_monitor_health_access_check_failed()
        })?;

    let mut coarse_fallback_ids = Vec::new();
    for project_id in &unique_project_ids {
        match permissions.get(project_id) {
            Some(Some(project_permissions)) => {
                if !project_permissions
                    .iter()
                    .any(|permission| permission == &required_permission)
                {
                    return Err(projects_monitor_health_access_denied(
                        PermissionDenialKind::ProjectPermission,
                    ));
                }
            }
            Some(None) => coarse_fallback_ids.push(*project_id),
            None => {
                tracing::error!(
                    user_id,
                    project_ids = ?unique_project_ids,
                    omitted_project_id = *project_id,
                    "status-page bulk project permission result was incomplete"
                );
                return Err(projects_monitor_health_access_check_failed());
            }
        }
    }

    if coarse_fallback_ids.is_empty() {
        return Ok(());
    }

    let coarse_access = checker
        .user_can_access_projects(user_id, &coarse_fallback_ids)
        .await
        .map_err(|error| {
            tracing::error!(
                user_id,
                project_ids = ?coarse_fallback_ids,
                error = %error,
                "status-page bulk coarse project access check failed"
            );
            projects_monitor_health_access_check_failed()
        })?;

    for project_id in coarse_fallback_ids {
        match coarse_access.get(&project_id) {
            Some(true) => {}
            Some(false) => {
                return Err(projects_monitor_health_access_denied(
                    PermissionDenialKind::ProjectAccess,
                ));
            }
            None => {
                tracing::error!(
                    user_id,
                    project_id,
                    "status-page bulk coarse project access result was incomplete"
                );
                return Err(projects_monitor_health_access_check_failed());
            }
        }
    }

    Ok(())
}

fn projects_monitor_health_access_denied(kind: PermissionDenialKind) -> Problem {
    forbidden()
        .type_("https://temps.sh/probs/project-access-denied")
        .title("Project Access Denied")
        .detail("You do not have access to all requested projects")
        .permission_denial(kind, Some("status_page:read".to_string()))
        .build()
}

fn projects_monitor_health_access_check_failed() -> Problem {
    internal_server_error()
        .type_("https://temps.sh/probs/project-access-check-failed")
        .title("Project Access Check Failed")
        .detail("Could not verify project access; please try again")
        .build()
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
        .route(
            "/monitors-health/projects",
            get(get_projects_monitor_health),
        )
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
        StatusPageError::HttpClientBuild { source } => {
            tracing::error!(error = %source, "status monitor HTTP client initialization failed");
            internal_server_error()
                .detail("Status monitoring is temporarily unavailable")
                .build()
        }
        StatusPageError::Internal(msg) => {
            tracing::error!("Internal error: {}", msg);
            internal_server_error().detail(&msg).build()
        }
        StatusPageError::EnvironmentNotInProject {
            environment_id,
            project_id,
        } => bad_request()
            .detail(format!(
                "Environment {environment_id} does not belong to project {project_id}"
            ))
            .build(),
        StatusPageError::MonitorNotInProject {
            monitor_id,
            project_id,
        } => bad_request()
            .detail(format!(
                "Monitor {monitor_id} does not belong to project {project_id}"
            ))
            .build(),
        error @ (StatusPageError::EnvironmentOwnershipLookup { .. }
        | StatusPageError::MonitorOwnershipLookup { .. }) => {
            tracing::error!(error = %error, "status-page association ownership lookup failed");
            internal_server_error()
                .detail("Could not validate status-page resource ownership")
                .build()
        }
    }
}

async fn record_status_page_audit<T>(app_state: &T, audit: StatusPageMutationAudit)
where
    T: StatusPageAppState,
{
    if let Err(error) = app_state.audit_service().create_audit_log(&audit).await {
        tracing::error!(
            project_id = audit.project_id,
            resource_type = audit.resource_type,
            resource_id = audit.resource_id,
            action = ?audit.action,
            error = %error,
            "failed to persist status-page mutation audit"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{ActiveModelTrait, Set};
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::{Permission, Role};
    use temps_config::ConfigService;
    use temps_core::{NoopTelemetryReporter, ProjectAccessChecker, TelemetryReporter};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{
        deployment_tokens::DeploymentTokenPermission, environments, projects,
        upstream_config::UpstreamList, users,
    };

    /// Mock team-based project access checker: allows only the projects
    /// listed in `allowed`, mirroring the plugin `TeamProjectAccessChecker`
    /// registers when EE Teams is installed.
    #[derive(Clone)]
    struct MockProjectAccessChecker {
        allowed: Vec<i32>,
        effective_permissions: Option<Vec<String>>,
    }

    #[async_trait]
    impl ProjectAccessChecker for MockProjectAccessChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.allowed.contains(&project_id))
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            if self.allowed.contains(&project_id) {
                Ok(self.effective_permissions.clone())
            } else {
                Ok(Some(Vec::new()))
            }
        }
    }

    /// Records calls to the batch authorization surface so the bulk-health
    /// regression tests fail if the route regresses to one check per project.
    struct BatchProjectAccessChecker {
        permissions: BTreeMap<i32, Option<Vec<String>>>,
        coarse_access: BTreeMap<i32, bool>,
        fail_permissions: bool,
        fail_coarse_access: bool,
        permission_calls: AtomicUsize,
        coarse_calls: AtomicUsize,
        permission_requests: Mutex<Vec<Vec<i32>>>,
        coarse_requests: Mutex<Vec<Vec<i32>>>,
    }

    impl BatchProjectAccessChecker {
        fn new(
            permissions: BTreeMap<i32, Option<Vec<String>>>,
            coarse_access: BTreeMap<i32, bool>,
        ) -> Self {
            Self {
                permissions,
                coarse_access,
                fail_permissions: false,
                fail_coarse_access: false,
                permission_calls: AtomicUsize::new(0),
                coarse_calls: AtomicUsize::new(0),
                permission_requests: Mutex::new(Vec::new()),
                coarse_requests: Mutex::new(Vec::new()),
            }
        }

        fn failing_permissions() -> Self {
            let mut checker = Self::new(BTreeMap::new(), BTreeMap::new());
            checker.fail_permissions = true;
            checker
        }
    }

    #[async_trait]
    impl ProjectAccessChecker for BatchProjectAccessChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self
                .coarse_access
                .get(&project_id)
                .copied()
                .unwrap_or(false))
        }

        async fn user_can_access_projects(
            &self,
            _user_id: i32,
            project_ids: &[i32],
        ) -> Result<BTreeMap<i32, bool>, Box<dyn std::error::Error + Send + Sync>> {
            self.coarse_calls.fetch_add(1, Ordering::SeqCst);
            self.coarse_requests
                .lock()
                .expect("record coarse batch request")
                .push(project_ids.to_vec());
            if self.fail_coarse_access {
                return Err("coarse project access unavailable".into());
            }
            Ok(project_ids
                .iter()
                .filter_map(|project_id| {
                    self.coarse_access
                        .get(project_id)
                        .copied()
                        .map(|allowed| (*project_id, allowed))
                })
                .collect())
        }

        async fn effective_project_permissions_batch(
            &self,
            _user_id: i32,
            project_ids: &[i32],
        ) -> Result<BTreeMap<i32, Option<Vec<String>>>, Box<dyn std::error::Error + Send + Sync>>
        {
            self.permission_calls.fetch_add(1, Ordering::SeqCst);
            self.permission_requests
                .lock()
                .expect("record permission batch request")
                .push(project_ids.to_vec());
            if self.fail_permissions {
                return Err("project permission resolution unavailable".into());
            }
            Ok(project_ids
                .iter()
                .filter_map(|project_id| {
                    self.permissions
                        .get(project_id)
                        .cloned()
                        .map(|permissions| (*project_id, permissions))
                })
                .collect())
        }
    }

    struct TestAppState {
        status_page_service: StatusPageService,
        telemetry: Arc<dyn TelemetryReporter>,
        audit_service: Arc<dyn AuditLogger>,
        project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
    }

    impl StatusPageAppState for TestAppState {
        fn status_page_service(&self) -> &StatusPageService {
            &self.status_page_service
        }

        fn telemetry(&self) -> &Arc<dyn TelemetryReporter> {
            &self.telemetry
        }

        fn audit_service(&self) -> &Arc<dyn AuditLogger> {
            &self.audit_service
        }

        fn project_access_checker(&self) -> Option<Arc<dyn ProjectAccessChecker>> {
            self.project_access_checker.clone()
        }
    }

    #[derive(Default)]
    struct NoopAuditLogger;

    #[async_trait]
    impl AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FailingAuditLogger {
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AuditLogger for FailingAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("simulated audit persistence failure"))
        }
    }

    fn test_request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "192.0.2.25".to_string(),
            user_agent: "status-page-route-test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    fn test_user() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
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
        }
    }

    /// A non-admin caller holding exactly `StatusPageDelete` -- lets the
    /// delete-monitor guard test exercise `project_access_guard!` in
    /// isolation, since `Role::User` doesn't hold `StatusPageDelete` (would
    /// be rejected earlier by `permission_guard!`) and `Role::Admin` would
    /// bypass `project_access_guard!` entirely.
    fn status_page_delete_api_key_auth() -> AuthContext {
        AuthContext::new_api_key(
            test_user(),
            None,
            Some(vec![Permission::StatusPageDelete]),
            "status-page-delete-key".to_string(),
            1,
        )
    }

    fn create_mock_config_service(db: &Arc<sea_orm::DatabaseConnection>) -> Arc<ConfigService> {
        use temps_config::ServerConfig;
        let config = ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgres://test:test@localhost/test".to_string(),
            None,
            None,
        )
        .expect("Failed to create test config");
        Arc::new(ConfigService::new(Arc::new(config), db.clone()))
    }

    async fn create_test_project(db: &Arc<sea_orm::DatabaseConnection>) -> projects::Model {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let slug = format!("test-project-{}", nanos);
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set(slug.clone()),
            directory: Set(slug),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Nixpacks),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            ..Default::default()
        };
        project.insert(db.as_ref()).await.unwrap()
    }

    async fn create_test_environment(
        db: &Arc<sea_orm::DatabaseConnection>,
        project_id: i32,
    ) -> environments::Model {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let subdomain = format!("test-env-{}", nanos);
        let slug = format!("test-env-{}", nanos);
        let env = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set(slug.clone()),
            slug: Set(slug),
            subdomain: Set(subdomain.clone()),
            host: Set(format!("{}.local", subdomain)),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        };
        env.insert(db.as_ref()).await.unwrap()
    }

    /// Live project + monitor + incident, wired to an `AppState` backed by
    /// the checker `make_checker` builds from the real project id. Keeps the
    /// `TestDatabase` alive for the fixture's lifetime -- dropping it early
    /// would tear down the schema mid-test.
    struct Fixture {
        _db: TestDatabase,
        app_state: Arc<TestAppState>,
        project_id: i32,
        environment_id: i32,
        monitor_id: i32,
        incident_id: i32,
    }

    async fn build_fixture(
        make_checker: impl FnOnce(i32) -> Option<Arc<dyn ProjectAccessChecker>>,
    ) -> Fixture {
        build_fixture_with_audit(make_checker, Arc::new(NoopAuditLogger)).await
    }

    async fn build_fixture_with_audit(
        make_checker: impl FnOnce(i32) -> Option<Arc<dyn ProjectAccessChecker>>,
        audit_service: Arc<dyn AuditLogger>,
    ) -> Fixture {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();
        let config_service = create_mock_config_service(&db);
        let status_page_service = StatusPageService::new(db.clone(), config_service);

        let project = create_test_project(&db).await;
        let environment = create_test_environment(&db, project.id).await;

        let monitor = status_page_service
            .monitor_service()
            .create_monitor(
                project.id,
                CreateMonitorRequest {
                    name: "Test Monitor".to_string(),
                    monitor_type: "web".to_string(),
                    environment_id: environment.id,
                    check_interval_seconds: Some(60),
                    check_path: None,
                },
            )
            .await
            .unwrap();

        let incident = status_page_service
            .incident_service()
            .create_incident(
                project.id,
                CreateIncidentRequest {
                    title: "Test Incident".to_string(),
                    description: None,
                    severity: "minor".to_string(),
                    environment_id: Some(environment.id),
                    monitor_id: Some(monitor.id),
                },
            )
            .await
            .unwrap();

        let app_state = Arc::new(TestAppState {
            status_page_service,
            telemetry: Arc::new(NoopTelemetryReporter),
            audit_service,
            project_access_checker: make_checker(project.id),
        });

        Fixture {
            _db: test_db,
            app_state,
            project_id: project.id,
            environment_id: environment.id,
            monitor_id: monitor.id,
            incident_id: incident.id,
        }
    }

    /// A checker that is registered but denies every project -- simulates a
    /// user authenticated on the instance who is not a member of the team
    /// that owns this monitor/incident's project.
    fn denies_everything(_project_id: i32) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(MockProjectAccessChecker {
            allowed: vec![],
            effective_permissions: None,
        }))
    }

    /// A checker that grants access to exactly this fixture's project --
    /// simulates a user who *is* a member of the owning team.
    fn allows_this_project(project_id: i32) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(MockProjectAccessChecker {
            allowed: vec![project_id],
            effective_permissions: None,
        }))
    }

    /// Coarse project membership paired with the effective permissions of a
    /// project viewer. This reproduces the bypass: the old
    /// `permission_guard!` + `project_access_guard!` pair allowed the caller's
    /// broad instance role to authorize mutations that the project role did
    /// not grant.
    fn viewer_for_this_project(project_id: i32) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(MockProjectAccessChecker {
            allowed: vec![project_id],
            effective_permissions: Some(vec![Permission::StatusPageRead.to_string()]),
        }))
    }

    /// Effective status-page permissions of a project admin, used to prove
    /// that the permission-specific guard preserves the intended mutation
    /// paths for sufficiently privileged project members.
    fn status_page_admin_for_this_project(
        project_id: i32,
    ) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(MockProjectAccessChecker {
            allowed: vec![project_id],
            effective_permissions: Some(vec![
                Permission::StatusPageRead.to_string(),
                Permission::StatusPageWrite.to_string(),
                Permission::StatusPageCreate.to_string(),
                Permission::StatusPageDelete.to_string(),
            ]),
        }))
    }

    fn assert_project_permission_denied(problem: Problem, required_permission: &str) {
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            problem.body.get("type").and_then(|value| value.as_str()),
            Some("https://temps.sh/probs/project-permission-denied")
        );
        assert_eq!(
            problem
                .body
                .get("required_permission")
                .and_then(|value| value.as_str()),
            Some(required_permission)
        );
    }

    /// Regression for the complete status-page mutation inventory. A caller
    /// can be a coarse member of the project and hold broad instance-level
    /// mutation permissions while their effective project role is read-only;
    /// every status-page mutation must still be denied.
    #[tokio::test]
    async fn status_page_mutations_deny_effective_project_viewer() {
        let fx = build_fixture(viewer_for_this_project).await;

        let create_monitor_error = create_monitor(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.project_id),
            Json(CreateMonitorRequest {
                name: "Forbidden Monitor".to_string(),
                monitor_type: "web".to_string(),
                environment_id: fx.environment_id,
                check_interval_seconds: Some(60),
                check_path: None,
            }),
        )
        .await
        .err()
        .expect("an effective project viewer must not create a monitor");
        assert_project_permission_denied(create_monitor_error, "status_page:create");

        let create_incident_error = create_incident(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.project_id),
            Json(CreateIncidentRequest {
                title: "Forbidden Incident".to_string(),
                description: None,
                severity: "minor".to_string(),
                environment_id: Some(fx.environment_id),
                monitor_id: Some(fx.monitor_id),
            }),
        )
        .await
        .err()
        .expect("an effective project viewer must not create an incident");
        assert_project_permission_denied(create_incident_error, "status_page:create");

        let update_incident_error = update_incident_status(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.incident_id),
            Json(UpdateIncidentStatusRequest {
                status: "resolved".to_string(),
                message: "forbidden update".to_string(),
            }),
        )
        .await
        .err()
        .expect("an effective project viewer must not update an incident");
        assert_project_permission_denied(update_incident_error, "status_page:write");

        let delete_monitor_error = delete_monitor(
            RequireAuth(status_page_delete_api_key_auth()),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.monitor_id),
        )
        .await
        .err()
        .expect("an effective project viewer must not delete a monitor");
        assert_project_permission_denied(delete_monitor_error, "status_page:delete");
    }

    /// A project role containing the requested permissions continues to
    /// create monitors/incidents, update incidents, and delete monitors.
    #[tokio::test]
    async fn status_page_mutations_allow_effective_project_admin() {
        let fx = build_fixture(status_page_admin_for_this_project).await;

        let deletable_monitor = fx
            .app_state
            .status_page_service()
            .monitor_service()
            .create_monitor(
                fx.project_id,
                CreateMonitorRequest {
                    name: "Deletable Monitor".to_string(),
                    monitor_type: "web".to_string(),
                    environment_id: fx.environment_id,
                    check_interval_seconds: Some(60),
                    check_path: None,
                },
            )
            .await
            .expect("fixture should create a monitor for the delete path");

        let create_monitor_result = create_monitor(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.project_id),
            Json(CreateMonitorRequest {
                name: "Allowed Monitor".to_string(),
                monitor_type: "web".to_string(),
                environment_id: fx.environment_id,
                check_interval_seconds: Some(60),
                check_path: None,
            }),
        )
        .await;
        assert!(
            create_monitor_result.is_ok(),
            "a project admin should be able to create a monitor"
        );

        let create_incident_result = create_incident(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.project_id),
            Json(CreateIncidentRequest {
                title: "Allowed Incident".to_string(),
                description: None,
                severity: "minor".to_string(),
                environment_id: Some(fx.environment_id),
                monitor_id: Some(fx.monitor_id),
            }),
        )
        .await;
        assert!(
            create_incident_result.is_ok(),
            "a project admin should be able to create an incident"
        );

        let update_incident_result = update_incident_status(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.incident_id),
            Json(UpdateIncidentStatusRequest {
                status: "resolved".to_string(),
                message: "authorized update".to_string(),
            }),
        )
        .await;
        assert!(
            update_incident_result.is_ok(),
            "a project admin should be able to update an incident"
        );

        let delete_monitor_result = delete_monitor(
            RequireAuth(status_page_delete_api_key_auth()),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(deletable_monitor.id),
        )
        .await;
        assert!(
            delete_monitor_result.is_ok(),
            "a project admin should be able to delete a monitor"
        );
    }

    #[tokio::test]
    async fn status_page_mutations_succeed_when_audit_persistence_fails() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let audit_service: Arc<dyn AuditLogger> = Arc::new(FailingAuditLogger {
            attempts: attempts.clone(),
        });
        let fx = build_fixture_with_audit(status_page_admin_for_this_project, audit_service).await;

        let deletable_monitor = fx
            .app_state
            .status_page_service()
            .monitor_service()
            .create_monitor(
                fx.project_id,
                CreateMonitorRequest {
                    name: "Audit failure delete target".to_string(),
                    monitor_type: "web".to_string(),
                    environment_id: fx.environment_id,
                    check_interval_seconds: Some(60),
                    check_path: None,
                },
            )
            .await
            .expect("fixture should create delete target");

        let create_monitor_result = create_monitor(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.project_id),
            Json(CreateMonitorRequest {
                name: "Audit failure monitor".to_string(),
                monitor_type: "web".to_string(),
                environment_id: fx.environment_id,
                check_interval_seconds: Some(60),
                check_path: None,
            }),
        )
        .await;
        assert!(create_monitor_result.is_ok());

        let create_incident_result = create_incident(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.project_id),
            Json(CreateIncidentRequest {
                title: "Audit failure incident".to_string(),
                description: None,
                severity: "minor".to_string(),
                environment_id: Some(fx.environment_id),
                monitor_id: Some(fx.monitor_id),
            }),
        )
        .await;
        assert!(create_incident_result.is_ok());

        let update_result = update_incident_status(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.incident_id),
            Json(UpdateIncidentStatusRequest {
                status: "monitoring".to_string(),
                message: "audit logger unavailable".to_string(),
            }),
        )
        .await;
        assert!(update_result.is_ok());

        let delete_result = delete_monitor(
            RequireAuth(status_page_delete_api_key_auth()),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(deletable_monitor.id),
        )
        .await;
        assert!(delete_result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn delete_monitor_denies_missing_coarse_permission_before_id_lookup() {
        let fx = build_fixture(allows_this_project).await;

        let problem = delete_monitor(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(i32::MAX),
        )
        .await
        .err()
        .expect("user role lacks status-page delete permission");

        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            problem.body.get("type").and_then(|value| value.as_str()),
            Some("https://temps.sh/probs/insufficient-permissions")
        );
    }

    #[tokio::test]
    async fn update_incident_denies_missing_coarse_permission_before_id_lookup() {
        let fx = build_fixture(allows_this_project).await;

        let problem = update_incident_status(
            RequireAuth(AuthContext::new_session(test_user(), Role::Reader)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(i32::MAX),
            Json(UpdateIncidentStatusRequest {
                status: "resolved".to_string(),
                message: "must not reach ownership lookup".to_string(),
            }),
        )
        .await
        .err()
        .expect("reader role lacks status-page write permission");

        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            problem.body.get("type").and_then(|value| value.as_str()),
            Some("https://temps.sh/probs/insufficient-permissions")
        );
    }

    /// Regression: before the fix, `get_monitor` never called
    /// `project_access_guard!`, so any authenticated user with
    /// `StatusPageRead` could read another team's monitor by id.
    #[tokio::test]
    async fn get_monitor_denies_user_without_project_access() {
        let fx = build_fixture(denies_everything).await;

        let rejection = get_monitor(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Path(fx.monitor_id),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(
            rejection,
            Some(StatusCode::FORBIDDEN),
            "a user without team access to the monitor's project must be denied"
        );
    }

    /// Sanity check for the happy path: a user whose team the checker grants
    /// access to must still be able to read the monitor.
    #[tokio::test]
    async fn get_monitor_allows_user_with_project_access() {
        let fx = build_fixture(allows_this_project).await;

        let rejection = get_monitor(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Path(fx.monitor_id),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_ne!(
            rejection,
            Some(StatusCode::FORBIDDEN),
            "a user with team access to the monitor's project must not be denied"
        );
    }

    /// Regression: `delete_monitor` never called `project_access_guard!`.
    /// Uses an API-key caller scoped to exactly `StatusPageDelete` so the
    /// project-access check is exercised in isolation from both the
    /// instance-wide permission gate and the admin bypass.
    #[tokio::test]
    async fn delete_monitor_denies_caller_without_project_access() {
        let fx = build_fixture(denies_everything).await;

        let rejection = delete_monitor(
            RequireAuth(status_page_delete_api_key_auth()),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.monitor_id),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(
            rejection,
            Some(StatusCode::FORBIDDEN),
            "a caller without team access to the monitor's project must be denied"
        );
    }

    /// Regression: `get_current_monitor_status` never called
    /// `project_access_guard!`.
    #[tokio::test]
    async fn get_current_monitor_status_denies_user_without_project_access() {
        let fx = build_fixture(denies_everything).await;

        let rejection = get_current_monitor_status(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Path(fx.monitor_id),
            Query(CurrentStatusQuery {
                start_time: None,
                end_time: None,
            }),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(rejection, Some(StatusCode::FORBIDDEN));
    }

    /// Regression: `get_uptime_history` never called `project_access_guard!`.
    #[tokio::test]
    async fn get_uptime_history_denies_user_without_project_access() {
        let fx = build_fixture(denies_everything).await;

        let rejection = get_uptime_history(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Path(fx.monitor_id),
            Query(UptimeQuery {
                days: None,
                start_time: None,
                end_time: None,
            }),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(rejection, Some(StatusCode::FORBIDDEN));
    }

    /// Regression: `get_bucketed_status` never called `project_access_guard!`.
    #[tokio::test]
    async fn get_bucketed_status_denies_user_without_project_access() {
        let fx = build_fixture(denies_everything).await;

        let rejection = get_bucketed_status(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Path(fx.monitor_id),
            Query(BucketedQuery {
                interval: None,
                start_time: None,
                end_time: None,
            }),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(rejection, Some(StatusCode::FORBIDDEN));
    }

    /// Regression: before the fix, `get_incident` never called
    /// `project_access_guard!`, so any authenticated user with
    /// `StatusPageRead` could read another team's incident by id.
    #[tokio::test]
    async fn get_incident_denies_user_without_project_access() {
        let fx = build_fixture(denies_everything).await;

        let rejection = get_incident(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Path(fx.incident_id),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(
            rejection,
            Some(StatusCode::FORBIDDEN),
            "a user without team access to the incident's project must be denied"
        );
    }

    /// Regression: before the fix, `update_incident_status` never called
    /// `project_access_guard!`, so any authenticated user with
    /// `StatusPageWrite` could write a fabricated status onto another
    /// team's incident.
    #[tokio::test]
    async fn update_incident_status_denies_user_without_project_access() {
        let fx = build_fixture(denies_everything).await;

        let rejection = update_incident_status(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Extension(test_request_metadata()),
            Path(fx.incident_id),
            Json(UpdateIncidentStatusRequest {
                status: "resolved".to_string(),
                message: "forged update".to_string(),
            }),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(
            rejection,
            Some(StatusCode::FORBIDDEN),
            "a user without team access to the incident's project must not be able to write to it"
        );
    }

    /// Regression: `get_incident_updates` never called `project_access_guard!`.
    #[tokio::test]
    async fn get_incident_updates_denies_user_without_project_access() {
        let fx = build_fixture(denies_everything).await;

        let rejection = get_incident_updates(
            RequireAuth(AuthContext::new_session(test_user(), Role::User)),
            State(fx.app_state.clone()),
            Path(fx.incident_id),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(rejection, Some(StatusCode::FORBIDDEN));
    }

    #[tokio::test]
    async fn projects_monitor_health_denies_mixed_project_permissions_in_one_batch() {
        let checker = BatchProjectAccessChecker::new(
            BTreeMap::from([
                (7, Some(vec![Permission::StatusPageRead.to_string()])),
                (9, Some(Vec::new())),
            ]),
            BTreeMap::new(),
        );
        let auth = AuthContext::new_session(test_user(), Role::User);

        let problem = require_projects_monitor_health_access(&auth, Some(&checker), &[9, 7, 9])
            .await
            .expect_err("one unauthorized project must deny the whole health request");

        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            problem.body.get("detail").and_then(|value| value.as_str()),
            Some("You do not have access to all requested projects")
        );
        assert!(!problem.body.contains_key("project_id"));
        assert!(!problem.body.contains_key("project_ids"));
        assert_eq!(checker.permission_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checker.coarse_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            *checker
                .permission_requests
                .lock()
                .expect("read permission requests"),
            vec![vec![7, 9]],
            "duplicate ids must be resolved once in a deterministic batch"
        );
    }

    #[tokio::test]
    async fn projects_monitor_health_fails_closed_on_checker_error() {
        let checker = BatchProjectAccessChecker::failing_permissions();
        let auth = AuthContext::new_session(test_user(), Role::User);

        let problem = require_projects_monitor_health_access(&auth, Some(&checker), &[7, 9])
            .await
            .expect_err("checker infrastructure failure must fail closed");

        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            problem.body.get("detail").and_then(|value| value.as_str()),
            Some("Could not verify project access; please try again")
        );
        assert_eq!(checker.permission_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checker.coarse_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn projects_monitor_health_fails_closed_on_incomplete_batch_result() {
        let checker = BatchProjectAccessChecker::new(
            BTreeMap::from([(7, Some(vec![Permission::StatusPageRead.to_string()]))]),
            BTreeMap::new(),
        );
        let auth = AuthContext::new_session(test_user(), Role::User);

        let problem = require_projects_monitor_health_access(&auth, Some(&checker), &[7, 9])
            .await
            .expect_err("omitting a requested project must fail closed");

        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(checker.permission_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checker.coarse_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn projects_monitor_health_allows_read_permissions_with_one_deduplicated_batch() {
        let read = Some(vec![Permission::StatusPageRead.to_string()]);
        let checker = BatchProjectAccessChecker::new(
            BTreeMap::from([(7, read.clone()), (9, read)]),
            BTreeMap::new(),
        );
        let auth = AuthContext::new_session(test_user(), Role::User);

        require_projects_monitor_health_access(&auth, Some(&checker), &[9, 7, 9])
            .await
            .expect("read permission on every project should allow the request");

        assert_eq!(checker.permission_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checker.coarse_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            *checker
                .permission_requests
                .lock()
                .expect("read permission requests"),
            vec![vec![7, 9]]
        );
    }

    #[tokio::test]
    async fn projects_monitor_health_uses_one_coarse_batch_for_legacy_checker_answers() {
        let checker = BatchProjectAccessChecker::new(
            BTreeMap::from([(7, None), (9, None)]),
            BTreeMap::from([(7, true), (9, true)]),
        );
        let auth = AuthContext::new_session(test_user(), Role::User);

        require_projects_monitor_health_access(&auth, Some(&checker), &[9, 7])
            .await
            .expect("coarse access should preserve legacy checker behavior");

        assert_eq!(checker.permission_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checker.coarse_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *checker
                .coarse_requests
                .lock()
                .expect("read coarse requests"),
            vec![vec![7, 9]]
        );
    }

    #[tokio::test]
    async fn projects_monitor_health_admin_bypasses_failing_project_checker() {
        let checker = BatchProjectAccessChecker::failing_permissions();
        let auth = AuthContext::new_session(test_user(), Role::Admin);

        require_projects_monitor_health_access(&auth, Some(&checker), &[7, 9])
            .await
            .expect("instance admins are not restricted by project membership");

        assert_eq!(checker.permission_calls.load(Ordering::SeqCst), 0);
        assert_eq!(checker.coarse_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn projects_monitor_health_confines_deployment_token_to_its_project() {
        let auth = AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "status-page-test-token".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        );

        require_projects_monitor_health_access(&auth, None, &[7, 7])
            .await
            .expect("the token's own project remains in scope");

        let problem = require_projects_monitor_health_access(&auth, None, &[7, 9])
            .await
            .expect_err("a bulk token request must not cross its project boundary");
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            problem.body.get("detail").and_then(|value| value.as_str()),
            Some("You do not have access to all requested projects")
        );
        assert!(!problem.body.contains_key("project_id"));
        assert!(!problem.body.contains_key("project_ids"));
    }
}
