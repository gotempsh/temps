use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
    Json, Router,
};
use serde::Serialize;
use temps_auth::permissions::Role;
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};

use crate::service::{CreateProjectAccessRequest, GrantAuthz, ProjectAccessResponse};

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
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
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
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
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

// ---------------------------------------------------------------------------
// Authorization inputs for mutating a project's grants
// ---------------------------------------------------------------------------

/// Gathers what the service needs to authorize a grant change.
///
/// The rules themselves live in the service, evaluated inside a transaction
/// against grant rows locked `FOR UPDATE` — see [`GrantAuthz`]. They were
/// originally evaluated here, which had three defects, all from deciding
/// against an unlocked, cached snapshot:
///
/// - two concurrent revokes each read "not the last grant", both
///   authorized, and between them removed every grant, silently re-opening
///   the project to everyone;
/// - the permission lookup came from the 60 s cache, so a just-revoked
///   project admin could re-grant themselves and make the revocation
///   non-durable;
/// - only the incoming role was ceiling-checked, so a project-admin could
///   demote or delete an `owner` grant and become the top authority.
///
/// This function therefore resolves the caller's permissions **uncached**
/// and hands them over; it deliberately makes no decision itself.
async fn grant_authz(
    auth: &temps_auth::context::AuthContext,
    state: &TeamsAppState,
    project_id: i32,
) -> Result<GrantAuthz, Problem> {
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(GrantAuthz::instance_admin());
    }

    let held = match state
        .checker
        .effective_project_permissions_uncached(auth.user_id(), project_id)
        .await
    {
        // `None` means the project has no grants at all. That is reachable
        // here (a revoke against an ungated project, or a concurrent revoke
        // of the last grant between this call and the service's lock), and
        // it maps to "holds nothing", not "unrestricted" — the empty set
        // denies, which is the intended answer. Do not "simplify" this into
        // treating `None` as an allow.
        Ok(opt) => opt.unwrap_or_default().into_iter().collect(),
        Err(e) => {
            tracing::error!(
                project_id,
                user_id = auth.user_id(),
                error = %e,
                "teams: could not resolve caller permissions while authorizing a grant change"
            );
            return Err(temps_core::error_builder::ErrorBuilder::new(
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .type_("https://temps.sh/probs/project-access-check-failed")
            .title("Project Access Check Failed")
            .detail("Could not verify project access; please try again")
            .build());
        }
    };

    Ok(GrantAuthz {
        is_instance_admin: false,
        held,
    })
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
    // ...and the coarse guard above is not sufficient on its own: it passes
    // for a `viewer`, who could then rewrite this very grant. See
    // `authorize_grant_mutation`.
    let authz = grant_authz(&auth, &state, project_id).await?;
    // Capture audit fields before req is moved into the service call.
    let team_id = req.team_id;
    let role = req.role.to_string();
    let grant = state
        .team_service
        .grant_project_access(auth.user_id(), project_id, req, &authz)
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
    let authz = grant_authz(&auth, &state, project_id).await?;
    state
        .team_service
        .revoke_project_access(project_id, team_id, &authz)
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
