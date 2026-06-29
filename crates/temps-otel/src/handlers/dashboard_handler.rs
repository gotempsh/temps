//! CRUD handlers for per-project saved metric dashboards.
//!
//! Authenticated via the standard `RequireAuth` flow (JWT/session) since these
//! are accessed by the Temps dashboard UI, not by OTel collectors. GET uses the
//! `OtelRead` permission; writes use `OtelWrite` and are audit-logged
//! best-effort.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use tracing::error;
use utoipa::ToSchema;

use crate::handlers::audit::{
    OtelDashboardCreatedAudit, OtelDashboardDeletedAudit, OtelDashboardUpdatedAudit,
};
use crate::services::dashboard_service::DashboardLayout;
use crate::OtelAppState;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, ProblemDetails, RequestMetadata};
use temps_entities::metric_dashboards::Model;

// ── Request DTOs ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListDashboardsParams {
    pub project_id: i32,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateDashboardRequest {
    pub project_id: i32,
    pub name: String,
    pub layout: DashboardLayout,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateDashboardRequest {
    pub name: Option<String>,
    pub layout: Option<DashboardLayout>,
}

// ── Response DTOs ───────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct OtelDashboardResponse {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub layout: DashboardLayout,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub created_at: String,
    #[schema(example = "2025-10-12T12:15:47.609192Z")]
    pub updated_at: String,
}

impl From<Model> for OtelDashboardResponse {
    fn from(model: Model) -> Self {
        let id = model.id;
        // The layout column is JSON and is always written by the service from a
        // typed DashboardLayout, so a decode failure means a corrupted row. Do
        // not swallow it silently: log it at ERROR (with the id) so the
        // corruption is visible, and degrade to an empty layout so a single bad
        // row doesn't 500 a whole list.
        let layout = serde_json::from_value(model.layout).unwrap_or_else(|e| {
            error!(
                dashboard_id = id,
                error = %e,
                "Stored dashboard layout failed to decode; returning empty layout"
            );
            DashboardLayout {
                sections: Vec::new(),
            }
        });
        Self {
            id,
            project_id: model.project_id,
            name: model.name,
            layout,
            created_at: model.created_at.to_rfc3339(),
            updated_at: model.updated_at.to_rfc3339(),
        }
    }
}

/// Query params scoping a by-id dashboard operation to a project. Required on
/// get/update/delete so a caller cannot touch another project's dashboard by
/// guessing its id (cross-tenant IDOR).
#[derive(Debug, Deserialize)]
pub struct DashboardScopeParams {
    pub project_id: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OtelDashboardsResponse {
    pub data: Vec<OtelDashboardResponse>,
    pub total: u64,
}

// ── Handlers ────────────────────────────────────────────────────────

/// List dashboards for a project (newest first, paginated).
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/dashboards",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default: 20, max: 100)"),
    ),
    responses(
        (status = 200, description = "Dashboards for the project", body = OtelDashboardsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_dashboards(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Query(params): Query<ListDashboardsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let (items, total) = state
        .dashboard_service
        .list(params.project_id, params.page, params.page_size)
        .await?;

    let data = items.into_iter().map(OtelDashboardResponse::from).collect();
    Ok(Json(OtelDashboardsResponse { data, total }))
}

/// Create a new dashboard for a project.
#[utoipa::path(
    tag = "OTel",
    post,
    path = "/otel/dashboards",
    request_body = CreateDashboardRequest,
    responses(
        (status = 201, description = "Dashboard created", body = OtelDashboardResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_dashboard(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateDashboardRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);

    let model = state
        .dashboard_service
        .create(request.project_id, request.name, request.layout)
        .await?;

    let audit = OtelDashboardCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        dashboard_id: model.id,
        project_id: model.project_id,
        name: model.name.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((
        StatusCode::CREATED,
        Json(OtelDashboardResponse::from(model)),
    ))
}

/// Fetch a single dashboard by id.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/dashboards/{id}",
    params(
        ("id" = i32, Path, description = "Dashboard ID"),
        ("project_id" = i32, Query, description = "Owning project ID (scopes the lookup)"),
    ),
    responses(
        (status = 200, description = "Dashboard", body = OtelDashboardResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Dashboard not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_dashboard(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(id): Path<i32>,
    Query(scope): Query<DashboardScopeParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let model = state.dashboard_service.get(scope.project_id, id).await?;
    Ok(Json(OtelDashboardResponse::from(model)))
}

/// Update a dashboard's name and/or layout.
#[utoipa::path(
    tag = "OTel",
    patch,
    path = "/otel/dashboards/{id}",
    params(
        ("id" = i32, Path, description = "Dashboard ID"),
        ("project_id" = i32, Query, description = "Owning project ID (scopes the update)"),
    ),
    request_body = UpdateDashboardRequest,
    responses(
        (status = 200, description = "Dashboard updated", body = OtelDashboardResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Dashboard not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_dashboard(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Query(scope): Query<DashboardScopeParams>,
    Json(request): Json<UpdateDashboardRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);

    let model = state
        .dashboard_service
        .update(scope.project_id, id, request.name, request.layout)
        .await?;

    let audit = OtelDashboardUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        dashboard_id: model.id,
        project_id: model.project_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(OtelDashboardResponse::from(model)))
}

/// Delete a dashboard.
#[utoipa::path(
    tag = "OTel",
    delete,
    path = "/otel/dashboards/{id}",
    params(
        ("id" = i32, Path, description = "Dashboard ID"),
        ("project_id" = i32, Query, description = "Owning project ID (scopes the delete)"),
    ),
    responses(
        (status = 204, description = "Dashboard deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Dashboard not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_dashboard(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Query(scope): Query<DashboardScopeParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);

    state.dashboard_service.delete(scope.project_id, id).await?;

    let audit = OtelDashboardDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        dashboard_id: id,
        project_id: scope.project_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}
