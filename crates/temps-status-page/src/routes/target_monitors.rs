// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Monitors for targets outside Temps (ADR 108 D3, D4).
//!
//! Creating one needs `status_page:create` on the project, changing one
//! `status_page:write`, like the environment monitors. Reading, history and
//! deletion use the existing `/monitors/{monitor_id}` routes. Every write is
//! audit-logged without the request headers, body or URL path. Where checks
//! can run from (ADR 108 D5) needs `status_page:read`.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use temps_authz::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::error_builder::{bad_request, internal_server_error, not_found, ErrorBuilder};
use temps_core::problemdetails::Problem;
use temps_core::{AuditLogger, RequestMetadata};
use utoipa::OpenApi;

use super::audit::{TargetMonitorAction, TargetMonitorAudit};
use crate::services::checks::{
    CheckRequest, CheckRequestHeader, CheckSpec, CreateTargetMonitorRequest, DnsCheck,
    DnsCheckRecordType, HttpCheck, HttpMethod, KeywordAssertion, MonitorCheckKind,
    MonitorCheckView, MonitorLocationResult, MonitorLocationResults, ProbeLocationView,
    ProbeLocationsResponse, TargetMonitorError, TargetMonitorService, TcpCheck, TlsCheck,
    UpdateTargetMonitorRequest,
};
use crate::services::MonitorResponse;
use temps_uptime_probe::ProbeStatus;

pub struct TargetMonitorRoutesState {
    pub monitors: Arc<TargetMonitorService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<TargetMonitorError> for Problem {
    fn from(error: TargetMonitorError) -> Self {
        match error {
            TargetMonitorError::ProjectNotFound { .. } => not_found()
                .title("Project Not Found")
                .detail(error.to_string())
                .build(),
            TargetMonitorError::NotFound { .. } => not_found()
                .title("Monitor Not Found")
                .detail(error.to_string())
                .build(),
            TargetMonitorError::NotTargetMonitor { .. } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Not A Target Monitor")
                .detail(error.to_string())
                .build(),
            TargetMonitorError::Config { .. } => bad_request()
                .title("Invalid Monitor")
                .detail(error.to_string())
                .build(),
            TargetMonitorError::NonPublicTarget { .. } => bad_request()
                .title("Target Not Public")
                .detail(error.to_string())
                .build(),
            TargetMonitorError::TooMany { .. } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Too Many Target Monitors")
                .detail(error.to_string())
                .build(),
            TargetMonitorError::Concurrent { .. } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Monitor Changed")
                .detail(error.to_string())
                .build(),
            TargetMonitorError::Encryption { .. } | TargetMonitorError::StoredCheck { .. } => {
                tracing::error!(error = %error, "target monitor operation failed");
                internal_server_error()
                    .title("Monitor Settings Unavailable")
                    .detail(error.to_string())
                    .build()
            }
            TargetMonitorError::Database(_) => internal_server_error()
                .title("Database Error")
                .detail(error.to_string())
                .build(),
        }
    }
}

fn target_host(monitor: &MonitorResponse) -> Option<String> {
    let target = &monitor.check.as_ref()?.target;
    match monitor.check_kind {
        MonitorCheckKind::Http => reqwest::Url::parse(target)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string)),
        _ => Some(target.clone()),
    }
}

async fn record(
    state: &TargetMonitorRoutesState,
    auth: &temps_authz::AuthContext,
    metadata: &RequestMetadata,
    action: TargetMonitorAction,
    monitor: &MonitorResponse,
) {
    let entry = TargetMonitorAudit {
        actor_user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        action,
        project_id: monitor.project_id,
        monitor_id: monitor.id,
        check_kind: monitor.check_kind.as_str().to_string(),
        target_host: target_host(monitor),
        header_names: monitor
            .check
            .as_ref()
            .map(|check| check.header_names.clone())
            .unwrap_or_default(),
        is_active: monitor.is_active,
        probe_from_nodes: monitor.probe_from_nodes,
    };
    if let Err(error) = state.audit.create_audit_log(&entry).await {
        tracing::error!(
            project_id = monitor.project_id,
            monitor_id = monitor.id,
            action = ?action,
            error = %error,
            "failed to persist target monitor audit"
        );
    }
}

/// Monitor a target outside Temps
///
/// Checks a URL (`http`), a port (`tcp`), a TLS certificate (`tls`) or a
/// DNS record (`dns`) that is not a Temps environment. The target must have
/// a public address. The first check runs within about 15 seconds.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/monitors/targets",
    operation_id = "create_target_monitor",
    request_body = CreateTargetMonitorRequest,
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 201, description = "Monitor created", body = MonitorResponse),
        (status = 400, description = "Invalid target or check, or a non-public address"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 409, description = "The project has too many target monitors"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn create_target_monitor(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TargetMonitorRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateTargetMonitorRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        StatusPageCreate,
        project_id,
        state.project_access_checker.clone()
    );
    // A project-scoped key may only manage its own project's monitors.
    project_scope_guard!(auth, project_id);
    let monitor: MonitorResponse = state.monitors.create(project_id, request).await?.into();
    record(
        &state,
        &auth,
        &metadata,
        TargetMonitorAction::Created,
        &monitor,
    )
    .await;
    Ok((StatusCode::CREATED, Json(monitor)))
}

/// Change a monitor of a target outside Temps
///
/// Fields left out keep their value. `request` replaces every header and the
/// body. Changing the check may change its kind; the target must fit it.
#[utoipa::path(
    patch,
    path = "/projects/{project_id}/monitors/{monitor_id}/target",
    operation_id = "update_target_monitor",
    request_body = UpdateTargetMonitorRequest,
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("monitor_id" = i32, Path, description = "Monitor ID"),
    ),
    responses(
        (status = 200, description = "Monitor updated", body = MonitorResponse),
        (status = 400, description = "Invalid target or check, or a non-public address"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Monitor not found in the project"),
        (status = 409, description = "The monitor checks a Temps environment"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn update_target_monitor(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TargetMonitorRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, monitor_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateTargetMonitorRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        StatusPageWrite,
        project_id,
        state.project_access_checker.clone()
    );
    // A project-scoped key may only manage its own project's monitors.
    project_scope_guard!(auth, project_id);
    let monitor: MonitorResponse = state
        .monitors
        .update(project_id, monitor_id, request)
        .await?
        .into();
    record(
        &state,
        &auth,
        &metadata,
        TargetMonitorAction::Updated,
        &monitor,
    )
    .await;
    Ok(Json(monitor))
}

/// Where monitors can be checked from
///
/// The control plane, plus the online worker nodes a monitor with
/// `probe_from_nodes` would also be checked from (at most four, one per
/// `region` label first). `configured` is false when there is no worker
/// node, with what to do about it.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/monitors/probe-locations",
    operation_id = "get_monitor_probe_locations",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Probe locations", body = ProbeLocationsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_monitor_probe_locations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TargetMonitorRoutesState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        StatusPageRead,
        project_id,
        state.project_access_checker.clone()
    );
    Ok(Json(state.monitors.probe_locations().await?))
}

pub fn create_target_monitor_router(state: Arc<TargetMonitorRoutesState>) -> Router {
    Router::new()
        .route(
            "/projects/{project_id}/monitors/targets",
            post(create_target_monitor),
        )
        .route(
            "/projects/{project_id}/monitors/{monitor_id}/target",
            patch(update_target_monitor),
        )
        .route(
            "/projects/{project_id}/monitors/probe-locations",
            get(get_monitor_probe_locations),
        )
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_target_monitor,
        update_target_monitor,
        get_monitor_probe_locations
    ),
    components(schemas(
        CreateTargetMonitorRequest,
        UpdateTargetMonitorRequest,
        MonitorCheckView,
        MonitorCheckKind,
        CheckSpec,
        CheckRequest,
        CheckRequestHeader,
        HttpCheck,
        HttpMethod,
        KeywordAssertion,
        TcpCheck,
        TlsCheck,
        DnsCheck,
        DnsCheckRecordType,
        ProbeStatus,
        MonitorLocationResult,
        MonitorLocationResults,
        ProbeLocationView,
        ProbeLocationsResponse,
    ))
)]
pub struct TargetMonitorApiDoc;
