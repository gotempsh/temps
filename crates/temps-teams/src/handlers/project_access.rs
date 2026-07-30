use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
    Json, Router,
};
use serde::Serialize;
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};

use crate::service::{CreateProjectAccessRequest, ProjectAccessResponse};

use super::TeamsAppState;

// ---------------------------------------------------------------------------
// Audit event types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct ProjectAccessGrantedAudit {
    #[serde(flatten)]
    context: AuditContext,
    project_id: i32,
    team_id: i32,
    role: String,
}

impl AuditOperation for ProjectAccessGrantedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_ACCESS_GRANTED".to_string()
    }
    fn user_id(&self) -> i32 {
        self.context.user_id
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize ProjectAccessGrantedAudit: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
struct ProjectAccessRevokedAudit {
    #[serde(flatten)]
    context: AuditContext,
    project_id: i32,
    team_id: i32,
}

impl AuditOperation for ProjectAccessRevokedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_ACCESS_REVOKED".to_string()
    }
    fn user_id(&self) -> i32 {
        self.context.user_id
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize ProjectAccessRevokedAudit: {e}"))
    }
}

pub(crate) fn router() -> Router<Arc<TeamsAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/access",
            get(list_project_access).post(grant_project_access),
        )
        .route(
            "/projects/{project_id}/access/{team_id}",
            delete(revoke_project_access),
        )
}

#[utoipa::path(
    tag = "Teams",
    get,
    path = "/projects/{project_id}/access",
    params(("project_id" = i32, Path)),
    responses(
        (status = 200, description = "Access grants", body = [ProjectAccessResponse]),
        (status = 403, description = "Insufficient permissions or no access to this project"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_project_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    // Who a project is shared with is part of that project's data: a user
    // with no access to it must not be able to enumerate its teams.
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(state.checker.clone());
    project_access_guard!(auth, project_id, checker);
    let grants = state.team_service.list_project_access(project_id).await?;
    let responses: Result<Vec<_>, _> = grants
        .into_iter()
        .map(ProjectAccessResponse::from_model)
        .collect();
    Ok(Json(responses.map_err(Problem::from)?))
}

#[utoipa::path(
    tag = "Teams",
    post,
    path = "/projects/{project_id}/access",
    params(("project_id" = i32, Path)),
    request_body = CreateProjectAccessRequest,
    responses(
        (status = 201, description = "Access granted (idempotent upsert)", body = ProjectAccessResponse),
        (status = 403, description = "Insufficient permissions or no access to this project"),
        (status = 404, description = "Team not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn grant_project_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<CreateProjectAccessRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    // Without this, a user gated out of a project could still hand their
    // own team access to it — writing themselves back in.
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(state.checker.clone());
    project_access_guard!(auth, project_id, checker);
    // Capture audit fields before req is moved into the service call.
    let team_id = req.team_id;
    let role = req.role.to_string();
    let grant = state
        .team_service
        .grant_project_access(auth.user_id(), project_id, req)
        .await?;
    let audit = ProjectAccessGrantedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        team_id,
        role,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write project access grant audit log");
    }
    let response = ProjectAccessResponse::from_model(grant).map_err(Problem::from)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    tag = "Teams",
    delete,
    path = "/projects/{project_id}/access/{team_id}",
    params(("project_id" = i32, Path), ("team_id" = i32, Path)),
    responses(
        (status = 204, description = "Access revoked"),
        (status = 403, description = "Insufficient permissions or no access to this project"),
        (status = 404, description = "Grant not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_project_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path((project_id, team_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(state.checker.clone());
    project_access_guard!(auth, project_id, checker);
    state
        .team_service
        .revoke_project_access(project_id, team_id)
        .await?;
    let audit = ProjectAccessRevokedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        team_id,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write project access revoke audit log");
    }
    Ok(StatusCode::NO_CONTENT)
}
