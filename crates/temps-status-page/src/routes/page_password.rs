// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authenticated management of a status page's password (ADR 108 D2).
//!
//! Reading whether a page has one needs `status_page:read`; setting or
//! removing it needs `status_page:write` on every project of the page. Every
//! change is audit-logged, never with the password.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use temps_authz::{permission_guard, project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::error_builder::{bad_request, internal_server_error, not_found};
use temps_core::problemdetails::Problem;
use temps_core::{AuditLogger, RequestMetadata};
use utoipa::OpenApi;

use super::audit::StatusPagePasswordAudit;
use crate::services::page_access::{
    PageAccessError, PageAccessService, PagePasswordResponse, SetPagePasswordRequest,
};
use crate::services::PublicStatusPageService;

pub struct PagePasswordRoutesState {
    pub access: Arc<PageAccessService>,
    pub pages: Arc<PublicStatusPageService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<PageAccessError> for Problem {
    fn from(error: PageAccessError) -> Self {
        match error {
            PageAccessError::StatusPageNotFound { .. } => not_found()
                .title("Status Page Not Found")
                .detail(error.to_string())
                .build(),
            PageAccessError::Validation { .. } => bad_request()
                .title("Invalid Password")
                .detail(error.to_string())
                .build(),
            PageAccessError::Hash { .. } => internal_server_error()
                .title("Password Hashing Failed")
                .detail(error.to_string())
                .build(),
            PageAccessError::Database(_) => internal_server_error()
                .title("Database Error")
                .detail(error.to_string())
                .build(),
        }
    }
}

/// Whether a status page needs a password
#[utoipa::path(
    get,
    path = "/status-pages/{status_page_id}/password",
    operation_id = "get_status_page_password",
    params(("status_page_id" = i32, Path, description = "Status page ID")),
    responses(
        (status = 200, description = "Whether the page has a password", body = PagePasswordResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn get_page_password(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<PagePasswordRoutesState>>,
    Path(status_page_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    page_access_guard!(auth, state, StatusPageRead, status_page_id);
    Ok(Json(state.access.status(status_page_id).await?))
}

/// Set, change or remove a status page's password
///
/// With a password (8 to 256 characters), visitors must type it to read the
/// page, its JSON or subscribe, and the page's Atom feed is not served.
/// `null` makes the page open again. Any change signs every visitor out.
#[utoipa::path(
    put,
    path = "/status-pages/{status_page_id}/password",
    operation_id = "set_status_page_password",
    params(("status_page_id" = i32, Path, description = "Status page ID")),
    request_body = SetPagePasswordRequest,
    responses(
        (status = 200, description = "Password changed", body = PagePasswordResponse),
        (status = 400, description = "Invalid password"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Status page not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Status Page",
    security(("bearer_auth" = []))
)]
pub async fn set_page_password(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<PagePasswordRoutesState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(status_page_id): Path<i32>,
    Json(request): Json<SetPagePasswordRequest>,
) -> Result<impl IntoResponse, Problem> {
    let project_ids = page_access_guard!(auth, state, StatusPageWrite, status_page_id);
    let response = state.access.set_password(status_page_id, request).await?;
    let entry = StatusPagePasswordAudit {
        actor_user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        project_ids,
        status_page_id,
        has_password: response.has_password,
    };
    if let Err(error) = state.audit.create_audit_log(&entry).await {
        tracing::error!(
            status_page_id,
            error = %error,
            "failed to persist status page password audit"
        );
    }
    Ok(Json(response))
}

pub fn create_page_password_router(state: Arc<PagePasswordRoutesState>) -> Router {
    Router::new()
        .route(
            "/status-pages/{status_page_id}/password",
            get(get_page_password).put(set_page_password),
        )
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(get_page_password, set_page_password),
    components(schemas(PagePasswordResponse, SetPagePasswordRequest))
)]
pub struct PagePasswordApiDoc;
