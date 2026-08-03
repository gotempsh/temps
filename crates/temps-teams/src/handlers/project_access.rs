use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
    Json, Router,
};
use serde::Serialize;
use temps_auth::permissions::{Permission, Role};
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};

use temps_core::ProjectAccessChecker;
use temps_entities::TeamRole;

use crate::role_permissions::fixed_role_permissions;
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
// Authorization for mutating a project's grants
// ---------------------------------------------------------------------------

/// What a caller is trying to do to a project's access grants.
enum GrantMutation {
    /// Add or update a grant with this role.
    Set(TeamRole),
    /// Remove the grant held by this team.
    Remove { team_id: i32 },
}

/// Guards the two endpoints that change *who can reach a project*.
///
/// `project_access_guard!` alone is not enough here. It answers the coarse
/// question "is this caller a member of some team granted on this
/// project?", which is true for a `viewer`. Left at that, a read-only
/// member could rewrite their own team's grant to `admin` and escape the
/// `member role ∩ grant role` ceiling that is the entire point of the
/// model — three requests: 403 on `PUT /projects/{id}`, self-promote,
/// 200 on the same request.
///
/// The rules, in order:
///
/// 1. Instance admins are unrestricted, consistently with every other
///    guard in the platform.
/// 2. Changing a project's *gating state* — adding the first grant, or
///    revoking the last one — is an administrative act: the first grant
///    locks every non-member out, and removing the last re-opens the
///    project to everyone. Both require an instance admin. Without this,
///    any `Role::User` could seize an ungated project (the coarse checker
///    returns "allowed" for an ungated project, by design) or silently
///    re-open a gated one.
/// 3. Otherwise the caller must hold `projects:write` *within this
///    project* — an admin/owner grant — not merely instance-wide
///    `ProjectsWrite`, which `Role::User` has.
/// 4. A caller may not hand out a role carrying permissions they do not
///    themselves hold here. Same ceiling shape as
///    `enforce_custom_permission_ceiling` on API keys: privilege you don't
///    have is privilege you can't delegate.
async fn authorize_grant_mutation(
    auth: &temps_auth::context::AuthContext,
    state: &TeamsAppState,
    project_id: i32,
    mutation: GrantMutation,
) -> Result<(), Problem> {
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(());
    }

    let grants = state.team_service.list_project_access(project_id).await?;

    let changes_gating_state = match &mutation {
        // First grant on an unrestricted project → it becomes gated.
        GrantMutation::Set(_) => grants.is_empty(),
        // Removing the only grant → it becomes unrestricted again.
        GrantMutation::Remove { team_id } => grants.len() == 1 && grants[0].team_id == *team_id,
    };
    if changes_gating_state {
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/project-gating-requires-admin")
                .title("Administrator Required")
                .detail(
                    "Restricting a project to teams, or removing its last grant and \
                 re-opening it, changes who can reach the project and requires an \
                 instance administrator",
                )
                .build(),
        );
    }

    // The caller's own permissions *inside* this project.
    let held: std::collections::HashSet<String> = match state
        .checker
        .effective_project_permissions(auth.user_id(), project_id)
        .await
    {
        // `None` means the project is unrestricted — already handled above,
        // since an unrestricted project has no grants and so any mutation
        // is a gating change. Treat it as holding nothing rather than
        // everything.
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

    if !held.contains(&Permission::ProjectsWrite.to_string()) {
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/project-permission-denied")
                .title("Project Permission Denied")
                .detail(
                    "Changing who can reach this project requires the projects:write \
                 permission on it — an admin or owner grant",
                )
                .value("required_permission", Permission::ProjectsWrite.to_string())
                .build(),
        );
    }

    // Can't delegate what you don't hold.
    if let GrantMutation::Set(role) = mutation {
        if let Some(excess) = fixed_role_permissions(role)
            .into_iter()
            .find(|p| !held.contains(&p.to_string()))
        {
            return Err(
                temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                    .type_("https://temps.sh/probs/permission-ceiling-exceeded")
                    .title("Forbidden")
                    .detail(format!(
                        "Cannot grant the '{role}' role: it carries '{excess}', which you do \
                     not hold on this project"
                    ))
                    .value("error_code", "PERMISSION_CEILING_EXCEEDED")
                    .build(),
            );
        }
    }

    Ok(())
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
    authorize_grant_mutation(&auth, &state, project_id, GrantMutation::Set(req.role)).await?;
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
    authorize_grant_mutation(&auth, &state, project_id, GrantMutation::Remove { team_id }).await?;
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
