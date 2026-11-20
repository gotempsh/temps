// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated management of a status page's maintenance windows
//! (ADR 108 D2).
//!
//! Authorization follows the page routes: the instance permission, then the
//! permission on every project the page draws from. Reading needs
//! `status_page:read`; scheduling, editing and cancelling need
//! `status_page:write`. Every write is audit-logged.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use temps_authz::{permission_guard, project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::error_builder::{bad_request, internal_server_error, not_found, ErrorBuilder};
use temps_core::problemdetails::Problem;
use temps_core::{AuditLogger, RequestMetadata};
use utoipa::OpenApi;

use super::audit::{StatusPageMaintenanceAction, StatusPageMaintenanceAudit};
use crate::services::maintenance::{
    CreateMaintenanceRequest, MaintenanceError, MaintenanceListResponse, MaintenanceResponse,
    MaintenanceState, StatusPageMaintenanceService, UpdateMaintenanceRequest,
};
use crate::services::PublicStatusPageService;

pub struct MaintenanceRoutesState {
    pub maintenance: Arc<StatusPageMaintenanceService>,
    pub pages: Arc<PublicStatusPageService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<MaintenanceError> for Problem {
    fn from(error: MaintenanceError) -> Self {
        match error {
            MaintenanceError::StatusPageNotFound { .. } => not_found()
                .title("Status Page Not Found")
                .detail(error.to_string())
                .build(),
            MaintenanceError::NotFound { .. } => not_found()
                .title("Maintenance Window Not Found")
                .detail(error.to_string())
                .build(),
            MaintenanceError::Validation { .. } => bad_request()
                .title("Invalid Maintenance Window")
                .detail(error.to_string())
                .build(),
            MaintenanceError::Conflict { .. } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Maintenance Window Cannot Change")
                .detail(error.to_string())
                .build(),
            MaintenanceError::TooManyScheduled { .. } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Too Many Maintenance Windows")
                .detail(error.to_string())
                .build(),
            MaintenanceError::Database(_) => internal_server_error()
                .title("Database Error")
                .detail(error.to_string())
                .build(),
        }
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct MaintenanceListQuery {
    /// Page number, starting at 1 (default 1).
    pub page: Option<u64>,
    /// Items per page (default 20, max 100).
    pub page_size: Option<u64>,
}

async fn record(
    state: &MaintenanceRoutesState,
    auth: &temps_authz::AuthContext,
    metadata: &RequestMetadata,
    action: StatusPageMaintenanceAction,
    project_ids: Vec<i32>,
    window: &MaintenanceResponse,
) {
    let entry = StatusPageMaintenanceAudit {
        actor_user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        action,
        project_ids,
        status_page_id: window.status_page_id,
        maintenance_id: window.id,
        title: window.title.clone(),
        starts_at: window.starts_at,
        ends_at: window.ends_at,
        monitor_ids: window.monitor_ids.clone(),
    };
    if let Err(error) = state.audit.create_audit_log(&entry).await {
        tracing::error!(
            status_page_id = window.status_page_id,
            maintenance_id = window.id,
            action = ?action,
            error = %error,
            "failed to persist status page maintenance audit"
        );
    }
}

/// Maintenance windows of a status page
///
/// Newest first, cancelled and completed ones included.
#[utoipa::path(
    get,
    path = "/status-pages/{status_page_id}/maintenances",
    operation_id = "list_status_page_maintenances",
    params(("status_page_id" = i32, Path, description = "Status page ID"), MaintenanceListQuery),
    responses(
        (status = 200, description = "Maintenance windows, newest first", body = MaintenanceListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn list_maintenances(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<MaintenanceRoutesState>>,
    Path(status_page_id): Path<i32>,
    Query(query): Query<MaintenanceListQuery>,
) -> Result<impl IntoResponse, Problem> {
    page_access_guard!(auth, state, StatusPageRead, status_page_id);
    Ok(Json(
        state
            .maintenance
            .list(
                status_page_id,
                query.page.unwrap_or(1),
                query.page_size.unwrap_or(20),
            )
            .await?,
    ))
}

/// Schedule a maintenance window
///
/// While it runs, the covered monitors show "Under maintenance", their
/// checks are left out of uptime and no outage incident or alert is raised
/// for them. It is announced once, now, through the page's feed and
/// subscriptions. It cannot start in the past.
#[utoipa::path(
    post,
    path = "/status-pages/{status_page_id}/maintenances",
    operation_id = "create_status_page_maintenance",
    params(("status_page_id" = i32, Path, description = "Status page ID")),
    request_body = CreateMaintenanceRequest,
    responses(
        (status = 201, description = "Maintenance window scheduled", body = MaintenanceResponse),
        (status = 400, description = "Invalid window"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 409, description = "The page has too many open windows"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn create_maintenance(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<MaintenanceRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(status_page_id): Path<i32>,
    Json(request): Json<CreateMaintenanceRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    let window = state.maintenance.create(status_page_id, request).await?;
    record(
        &state,
        &auth,
        &metadata,
        StatusPageMaintenanceAction::Scheduled,
        project_ids,
        &window,
    )
    .await;
    Ok((StatusCode::CREATED, Json(window)))
}

/// A maintenance window
#[utoipa::path(
    get,
    path = "/status-pages/{status_page_id}/maintenances/{maintenance_id}",
    operation_id = "get_status_page_maintenance",
    params(
        ("status_page_id" = i32, Path, description = "Status page ID"),
        ("maintenance_id" = i32, Path, description = "Maintenance window ID"),
    ),
    responses(
        (status = 200, description = "Maintenance window", body = MaintenanceResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page or window not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_maintenance(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<MaintenanceRoutesState>>,
    Path((status_page_id, maintenance_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    page_access_guard!(auth, state, StatusPageRead, status_page_id);
    Ok(Json(
        state
            .maintenance
            .get(status_page_id, maintenance_id)
            .await?,
    ))
}

/// Change a maintenance window
///
/// Before it starts every field can change; once it has started, only the
/// title, description and end. A cancelled or completed window cannot
/// change. Changes are shown on the page but not announced again.
#[utoipa::path(
    patch,
    path = "/status-pages/{status_page_id}/maintenances/{maintenance_id}",
    operation_id = "update_status_page_maintenance",
    params(
        ("status_page_id" = i32, Path, description = "Status page ID"),
        ("maintenance_id" = i32, Path, description = "Maintenance window ID"),
    ),
    request_body = UpdateMaintenanceRequest,
    responses(
        (status = 200, description = "Maintenance window updated", body = MaintenanceResponse),
        (status = 400, description = "Invalid window"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page or window not found"),
        (status = 409, description = "The window can no longer change that way"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn update_maintenance(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<MaintenanceRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((status_page_id, maintenance_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateMaintenanceRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    let window = state
        .maintenance
        .update(status_page_id, maintenance_id, request)
        .await?;
    record(
        &state,
        &auth,
        &metadata,
        StatusPageMaintenanceAction::Updated,
        project_ids,
        &window,
    )
    .await;
    Ok(Json(window))
}

/// Cancel a maintenance window
///
/// A scheduled window never runs; one in progress ends now. Checks taken
/// while it ran stay out of uptime. The window is kept, marked cancelled.
#[utoipa::path(
    delete,
    path = "/status-pages/{status_page_id}/maintenances/{maintenance_id}",
    operation_id = "cancel_status_page_maintenance",
    params(
        ("status_page_id" = i32, Path, description = "Status page ID"),
        ("maintenance_id" = i32, Path, description = "Maintenance window ID"),
    ),
    responses(
        (status = 200, description = "Maintenance window cancelled", body = MaintenanceResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page or window not found"),
        (status = 409, description = "The window was already cancelled or has ended"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn cancel_maintenance(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<MaintenanceRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((status_page_id, maintenance_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    let window = state
        .maintenance
        .cancel(status_page_id, maintenance_id)
        .await?;
    record(
        &state,
        &auth,
        &metadata,
        StatusPageMaintenanceAction::Cancelled,
        project_ids,
        &window,
    )
    .await;
    Ok(Json(window))
}

pub fn create_maintenance_router(state: Arc<MaintenanceRoutesState>) -> Router {
    Router::new()
        .route(
            "/status-pages/{status_page_id}/maintenances",
            get(list_maintenances).post(create_maintenance),
        )
        .route(
            "/status-pages/{status_page_id}/maintenances/{maintenance_id}",
            get(get_maintenance)
                .patch(update_maintenance)
                .delete(cancel_maintenance),
        )
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_maintenances,
        create_maintenance,
        get_maintenance,
        update_maintenance,
        cancel_maintenance,
    ),
    components(schemas(
        MaintenanceResponse,
        MaintenanceListResponse,
        MaintenanceState,
        CreateMaintenanceRequest,
        UpdateMaintenanceRequest,
    ))
)]
pub struct MaintenanceApiDoc;
