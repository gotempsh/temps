// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for import operations

pub mod audit;
pub mod types;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use temps_auth::{permission_check, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, RequestMetadata};
use tracing::error;
use utoipa::OpenApi;

use types::{
    CreatePlanRequest, CreatePlanResponse, DiscoverRequest, DiscoverResponse, ExecuteImportRequest,
    ExecuteImportResponse, ImportSourceInfo, ImportStatusResponse,
};

/// Configure routes for the import API
pub fn configure_routes() -> Router<Arc<types::AppState>> {
    Router::new()
        .route("/imports/sources", get(list_sources))
        .route("/imports/discover", post(discover_workloads))
        .route("/imports/plan", post(create_plan))
        .route("/imports/execute", post(execute_import))
        .route("/imports/{session_id}", get(get_import_status))
}

/// List available import sources
#[utoipa::path(
    get,
    path = "/imports/sources",
    tag = "Imports",
    responses(
        (status = 200, description = "List of available import sources", body = Vec<ImportSourceInfo>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_sources(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsRead);

    let sources = state.import_orchestrator.list_sources().await?;
    Ok(Json(sources))
}

/// Discover workloads from a source
#[utoipa::path(
    post,
    path = "/imports/discover",
    tag = "Imports",
    request_body = DiscoverRequest,
    responses(
        (status = 200, description = "List of discovered workloads", body = DiscoverResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn discover_workloads(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
    Json(request): Json<types::DiscoverRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsRead);

    let workloads = state
        .import_orchestrator
        .discover(request.source, &request.credentials, request.selector)
        .await?;

    Ok(Json(types::DiscoverResponse { workloads }))
}

/// Create an import plan
#[utoipa::path(
    post,
    path = "/imports/plan",
    tag = "Imports",
    request_body = CreatePlanRequest,
    responses(
        (status = 200, description = "Import plan created", body = CreatePlanResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn create_plan(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
    Json(request): Json<types::CreatePlanRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsCreate);

    let result = state
        .import_orchestrator
        .create_plan(
            auth.user_id(),
            request.source,
            request.workload_id,
            &request.credentials,
            request.repository_id,
        )
        .await?;

    Ok(Json(result))
}

/// Execute an import
#[utoipa::path(
    post,
    path = "/imports/execute",
    tag = "Imports",
    request_body = ExecuteImportRequest,
    responses(
        (status = 202, description = "Import execution started", body = ExecuteImportResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn execute_import(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<types::ExecuteImportRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsCreate);

    let session_id = request.session_id.clone();
    let project_name = request.project_name.clone();
    let dry_run = request.dry_run.unwrap_or(false);

    let result = state
        .import_orchestrator
        .execute_import(
            auth.user_id(),
            request.session_id,
            request.project_name,
            request.preset,
            request.directory,
            request.main_branch,
            dry_run,
        )
        .await?;

    // This creates real projects/services/deployments/domains from
    // user-supplied platform credentials — an audit trail is required
    // regardless of whether the import itself succeeded.
    let audit_event = audit::ImportExecutedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        session_id,
        project_name,
        project_id: result.project_id,
        environment_id: result.environment_id,
        dry_run,
        succeeded: matches!(result.status, types::ImportExecutionStatus::Completed),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log for import execution: {}", e);
        // Continue — the import itself already ran; audit logging failure
        // must not undo or fail a real operation that already happened.
    }

    Ok((StatusCode::ACCEPTED, Json(result)))
}

/// Get import status
#[utoipa::path(
    get,
    path = "/imports/{session_id}",
    tag = "Imports",
    params(
        ("session_id" = String, Path, description = "Import session ID")
    ),
    responses(
        (status = 200, description = "Import status", body = ImportStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Import session not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_import_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsRead);

    let status = state
        .import_orchestrator
        .get_status(auth.user_id(), &session_id)
        .await?;
    Ok(Json(status))
}

/// OpenAPI documentation
#[derive(OpenApi)]
#[openapi(
    paths(
        list_sources,
        discover_workloads,
        create_plan,
        execute_import,
        get_import_status,
    ),
    components(schemas(
        types::ImportSourceInfo,
        types::ImportSourceCapabilities,
        types::DiscoverRequest,
        types::DiscoverResponse,
        types::CreatePlanRequest,
        types::CreatePlanResponse,
        types::ExecuteImportRequest,
        types::ExecuteImportResponse,
        types::ImportStatusResponse,
    )),
    tags(
        (name = "Imports", description = "Import workloads from external sources")
    )
)]
pub struct ImportApiDoc;
