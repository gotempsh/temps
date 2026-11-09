// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for internal access grants (ADR 100 D3, D7).
//!
//! A grant belongs to the project being called. Listing needs
//! `projects:read` on it; creating and deleting need `projects:write` on it,
//! never a deployment token, and creating also needs access to the calling
//! project, so nobody can name or probe a project they cannot see.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{
    deny_deployment_token, project_access_guard, project_permission_guard, project_scope_guard,
    RequireAuth,
};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditLogger, AuditOperation, RequestMetadata, UtcDateTime};
use tracing::{error, info};
use utoipa::{IntoParams, OpenApi, ToSchema};

use super::error::InternalAccessError;
use super::service::{GrantPage, GrantView, InternalAccessService, NewGrant};

pub struct InternalAccessAppState {
    pub service: Arc<InternalAccessService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

pub fn router(state: Arc<InternalAccessAppState>) -> Router {
    Router::new()
        .route(
            "/projects/{project_id}/internal-access",
            get(list_internal_access_grants).post(create_internal_access_grant),
        )
        .route(
            "/projects/{project_id}/internal-access/{grant_id}",
            delete(delete_internal_access_grant),
        )
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_internal_access_grants,
        create_internal_access_grant,
        delete_internal_access_grant
    ),
    components(schemas(
        InternalAccessGrantResponse,
        InternalAccessGrantListResponse,
        CreateInternalAccessGrantRequest
    )),
    tags((name = "Internal Access", description = "Which other projects may call a project's *.temps.local addresses"))
)]
pub struct InternalAccessApiDoc;

impl From<InternalAccessError> for Problem {
    fn from(error: InternalAccessError) -> Self {
        match error {
            InternalAccessError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            InternalAccessError::EnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            InternalAccessError::GrantNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Internal Access Grant Not Found")
                .with_detail(error.to_string()),
            InternalAccessError::SelfGrant { .. }
            | InternalAccessError::DescriptionTooLong { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Internal Access Grant")
                    .with_detail(error.to_string())
            }
            InternalAccessError::DuplicateGrant { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Internal Access Already Granted")
                .with_detail(error.to_string()),
            InternalAccessError::TooManyGrants { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Too Many Internal Access Grants")
                .with_detail(error.to_string()),
            InternalAccessError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ------------------------------------------------------------------ DTOs

/// One grant: who (a project, or one environment of it) may call what (this
/// project, or one environment of it).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InternalAccessGrantResponse {
    pub id: i32,
    pub target_project_id: i32,
    pub target_project_name: String,
    pub target_project_slug: String,
    /// `null`: every environment of the target project.
    pub target_environment_id: Option<i32>,
    pub target_environment_name: Option<String>,
    pub caller_project_id: i32,
    pub caller_project_name: String,
    pub caller_project_slug: String,
    /// `null`: every environment of the caller project.
    pub caller_environment_id: Option<i32>,
    pub caller_environment_name: Option<String>,
    pub description: Option<String>,
    pub created_by: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
}

impl From<GrantView> for InternalAccessGrantResponse {
    fn from(grant: GrantView) -> Self {
        Self {
            id: grant.id,
            target_project_id: grant.target_project_id,
            target_project_name: grant.target_project_name,
            target_project_slug: grant.target_project_slug,
            target_environment_id: grant.target_environment_id,
            target_environment_name: grant.target_environment_name,
            caller_project_id: grant.caller_project_id,
            caller_project_name: grant.caller_project_name,
            caller_project_slug: grant.caller_project_slug,
            caller_environment_id: grant.caller_environment_id,
            caller_environment_name: grant.caller_environment_name,
            description: grant.description,
            created_by: grant.created_by,
            created_at: grant.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InternalAccessGrantListResponse {
    pub items: Vec<InternalAccessGrantResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

impl From<GrantPage> for InternalAccessGrantListResponse {
    fn from(page: GrantPage) -> Self {
        Self {
            items: page.items.into_iter().map(Into::into).collect(),
            total: page.total,
            page: page.page,
            page_size: page.page_size,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateInternalAccessGrantRequest {
    /// The environment of this project that may be called; omit for every
    /// environment, present and future.
    pub target_environment_id: Option<i32>,
    /// The project whose workloads may call.
    pub caller_project_id: i32,
    /// Only this environment of the calling project; omit for every
    /// environment, present and future.
    pub caller_environment_id: Option<i32>,
    /// Why the grant exists, up to 200 characters.
    #[schema(example = "The checkout worker calls the orders API")]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListInternalAccessQuery {
    /// Only grants that cover this environment of the project: its own and
    /// the project-wide ones.
    pub environment_id: Option<i32>,
    /// 1-based page, default 1.
    pub page: Option<u64>,
    /// Default 20, at most 100.
    pub page_size: Option<u64>,
}

// ----------------------------------------------------------------- audit

#[derive(Debug, Clone, Serialize)]
struct AuditContext {
    user_id: Option<i32>,
    ip_address: Option<String>,
    user_agent: String,
}

#[derive(Debug, Clone, Serialize)]
struct GrantAudit {
    #[serde(skip)]
    operation: &'static str,
    context: AuditContext,
    grant_id: i32,
    target_project_id: i32,
    target_project_slug: String,
    target_environment_id: Option<i32>,
    caller_project_id: i32,
    caller_project_slug: String,
    caller_environment_id: Option<i32>,
    description: Option<String>,
}

impl GrantAudit {
    fn new(
        operation: &'static str,
        user_id: Option<i32>,
        metadata: &RequestMetadata,
        grant: &GrantView,
    ) -> Self {
        Self {
            operation,
            context: AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            grant_id: grant.id,
            target_project_id: grant.target_project_id,
            target_project_slug: grant.target_project_slug.clone(),
            target_environment_id: grant.target_environment_id,
            caller_project_id: grant.caller_project_id,
            caller_project_slug: grant.caller_project_slug.clone(),
            caller_environment_id: grant.caller_environment_id,
            description: grant.description.clone(),
        }
    }
}

impl AuditOperation for GrantAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        serde_json::to_string(self)
            .map_err(|source| temps_core::AuditError::serialize(self.operation, source))
    }
}

const GRANT_CREATED: &str = "INTERNAL_ACCESS_GRANT_CREATED";
const GRANT_DELETED: &str = "INTERNAL_ACCESS_GRANT_DELETED";

async fn write_audit(state: &InternalAccessAppState, audit: &GrantAudit) {
    // The write already succeeded; a failed audit entry is reported, not fatal.
    if let Err(e) = state.audit.create_audit_log(audit).await {
        error!(
            operation = audit.operation,
            grant_id = audit.grant_id,
            target_project_id = audit.target_project_id,
            error = %e,
            "failed to write the internal access audit entry"
        );
    }
}

// -------------------------------------------------------------- handlers

/// List the projects allowed to call this project's internal addresses
///
/// Newest first. With `environment_id`, only the grants that cover that
/// environment (its own and the project-wide ones).
#[utoipa::path(
    tag = "Internal Access",
    get,
    path = "/projects/{project_id}/internal-access",
    params(
        ("project_id" = i32, Path, description = "The project being called"),
        ListInternalAccessQuery
    ),
    responses(
        (status = 200, description = "Grants on the project", body = InternalAccessGrantListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_internal_access_grants(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InternalAccessAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListInternalAccessQuery>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);

    let page = state
        .service
        .list(
            project_id,
            query.environment_id,
            query.page,
            query.page_size,
        )
        .await?;
    Ok(Json(InternalAccessGrantListResponse::from(page)))
}

/// Allow another project to call this project's internal addresses
///
/// Workloads of the calling project (or of one environment of it) may then
/// call this project's `*.temps.local` addresses (or one environment's)
/// through the internal proxy. Workers apply it with their next route sync,
/// within seconds.
#[utoipa::path(
    tag = "Internal Access",
    post,
    path = "/projects/{project_id}/internal-access",
    params(("project_id" = i32, Path, description = "The project being called")),
    request_body = CreateInternalAccessGrantRequest,
    responses(
        (status = 201, description = "Grant created", body = InternalAccessGrantResponse),
        (status = 400, description = "Invalid grant", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions on either project", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 409, description = "Already granted, or too many grants", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_internal_access_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InternalAccessAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateInternalAccessGrantRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    // Naming a project in a grant requires being able to see it.
    project_access_guard!(
        auth,
        request.caller_project_id,
        state.project_access_checker
    );

    let user_id = auth.user_id_opt();
    let grant = state
        .service
        .create(
            project_id,
            NewGrant {
                target_environment_id: request.target_environment_id,
                caller_project_id: request.caller_project_id,
                caller_environment_id: request.caller_environment_id,
                description: request.description,
            },
            user_id,
        )
        .await?;
    info!(
        grant_id = grant.id,
        target_project_id = grant.target_project_id,
        target_environment_id = ?grant.target_environment_id,
        caller_project_id = grant.caller_project_id,
        caller_environment_id = ?grant.caller_environment_id,
        "internal access granted"
    );
    write_audit(
        &state,
        &GrantAudit::new(GRANT_CREATED, user_id, &metadata, &grant),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(InternalAccessGrantResponse::from(grant)),
    ))
}

/// Stop another project from calling this project's internal addresses
///
/// Workers refuse the caller again with their next route sync, within
/// seconds (ADR 100 D5).
#[utoipa::path(
    tag = "Internal Access",
    delete,
    path = "/projects/{project_id}/internal-access/{grant_id}",
    params(
        ("project_id" = i32, Path, description = "The project being called"),
        ("grant_id" = i32, Path, description = "Grant ID")
    ),
    responses(
        (status = 204, description = "Grant removed"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Grant not found on this project", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_internal_access_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InternalAccessAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, grant_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);

    let grant = state.service.delete(project_id, grant_id).await?;
    info!(
        grant_id,
        target_project_id = project_id,
        caller_project_id = grant.caller_project_id,
        "internal access revoked"
    );
    write_audit(
        &state,
        &GrantAudit::new(GRANT_DELETED, auth.user_id_opt(), &metadata, &grant),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
