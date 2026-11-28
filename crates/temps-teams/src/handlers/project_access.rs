// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Serialize;
use temps_auth::permissions::Role;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};

use crate::service::{
    CreateProjectAccessRequest, GrantAuthz, ProjectAccessResponse, UpdateProjectAccessRequest,
};

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
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[derive(Debug, Clone, Serialize)]
struct ProjectAccessUpdatedAudit {
    #[serde(flatten)]
    context: AuditContext,
    project_id: i32,
    team_id: i32,
    role: String,
}

impl AuditOperation for ProjectAccessUpdatedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_ACCESS_UPDATED".to_string()
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
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectAccessRevokedAudit {
    #[serde(flatten)]
    pub(crate) context: AuditContext,
    pub(crate) project_id: i32,
    pub(crate) team_id: i32,
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
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
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
            get(get_project_access)
                .patch(update_project_access)
                .delete(revoke_project_access),
        )
}

// ---------------------------------------------------------------------------
// Authorization inputs for mutating a project's grants
// ---------------------------------------------------------------------------

/// Classifies the caller for the service's authorization rules.
///
/// This is the *whole* of the handler's involvement in that decision. It
/// deliberately does not resolve the caller's project permissions: those
/// are read by the service inside its transaction, from the grant rows it
/// holds locked. Resolving them here would put the decision back on a
/// snapshot taken before the lock — which is what let two concurrent
/// revokes each believe they were not removing the last grant, and let a
/// just-revoked project admin re-grant themselves from a stale read.
fn grant_authz(auth: &temps_auth::context::AuthContext) -> GrantAuthz {
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        GrantAuthz::instance_admin()
    } else {
        GrantAuthz::project_scoped()
    }
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
    project_scope_guard!(auth, project_id);
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
        (status = 201, description = "Access granted", body = ProjectAccessResponse),
        (status = 403, description = "Insufficient permissions or no access to this project"),
        (status = 404, description = "Project or team not found"),
        (status = 409, description = "The team already has a grant on this project; change its role with PATCH /projects/{project_id}/access/{team_id}"),
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
    project_scope_guard!(auth, project_id);
    // Without this, a user gated out of a project could still hand their
    // own team access to it — writing themselves back in.
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(state.checker.clone());
    project_access_guard!(auth, project_id, checker);
    // ...and the coarse guard above is not sufficient on its own: it passes
    // for a `viewer`, who could then rewrite this very grant. The rule that
    // stops them lives in the service, under the row lock.
    let authz = grant_authz(&auth);
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

/// One team's grant on a project.
#[utoipa::path(
    tag = "Teams",
    get,
    path = "/projects/{project_id}/access/{team_id}",
    params(("project_id" = i32, Path), ("team_id" = i32, Path)),
    responses(
        (status = 200, description = "The grant", body = ProjectAccessResponse),
        (status = 403, description = "Insufficient permissions or no access to this project"),
        (status = 404, description = "Project not found, or the team has no grant on it"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path((project_id, team_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(state.checker.clone());
    project_access_guard!(auth, project_id, checker);
    let grant = state
        .team_service
        .get_project_access(project_id, team_id)
        .await?;
    Ok(Json(
        ProjectAccessResponse::from_model(grant).map_err(Problem::from)?,
    ))
}

/// Change the role of a team's existing grant on a project. The same rules
/// as granting apply: `projects:write` on the project, and neither the new
/// nor the replaced role above the caller's own.
#[utoipa::path(
    tag = "Teams",
    patch,
    path = "/projects/{project_id}/access/{team_id}",
    params(("project_id" = i32, Path), ("team_id" = i32, Path)),
    request_body = UpdateProjectAccessRequest,
    responses(
        (status = 200, description = "Grant updated", body = ProjectAccessResponse),
        (status = 403, description = "Insufficient permissions or no access to this project"),
        (status = 404, description = "Project not found, or the team has no grant on it"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_project_access(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path((project_id, team_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<UpdateProjectAccessRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(state.checker.clone());
    project_access_guard!(auth, project_id, checker);
    let authz = grant_authz(&auth);
    let role = req.role.to_string();
    let grant = state
        .team_service
        .update_project_access(auth.user_id(), project_id, team_id, req.role, &authz)
        .await?;
    let audit = ProjectAccessUpdatedAudit {
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
        tracing::error!(error = %e, "teams: failed to write project access update audit log");
    }
    Ok(Json(
        ProjectAccessResponse::from_model(grant).map_err(Problem::from)?,
    ))
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
    project_scope_guard!(auth, project_id);
    let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(state.checker.clone());
    project_access_guard!(auth, project_id, checker);
    let authz = grant_authz(&auth);
    state
        .team_service
        .revoke_project_access(auth.user_id(), project_id, team_id, &authz)
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

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::context::AuthContext;
    use temps_entities::users;

    fn user(id: i32) -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id,
            name: "test".into(),
            email: "test@example.com".into(),
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

    #[test]
    fn instance_admins_bypass_the_project_scoped_rules() {
        for role in [Role::Admin, Role::PlatformAdmin] {
            let auth = AuthContext::new_session(user(1), role.clone());
            assert!(
                grant_authz(&auth).is_instance_admin,
                "{role:?} is an instance administrator"
            );
        }
    }

    struct NoAudit;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn AuditOperation,
        ) -> temps_core::AuditResult<()> {
            Ok(())
        }
    }

    struct AllowSensitive;

    #[async_trait::async_trait]
    impl temps_core::SensitiveActionAuthorizer for AllowSensitive {
        async fn authorize(
            &self,
            _action: &temps_core::SensitiveAction,
            _principal: &temps_core::SensitiveActionPrincipal,
        ) -> Result<
            temps_core::SensitiveActionDecision,
            temps_core::SensitiveActionAuthorizationError,
        > {
            Ok(temps_core::SensitiveActionDecision::Allow)
        }
    }

    /// No query results: a handler that reached the database would fail on
    /// it, so a 403 here can only come from the guard.
    fn state() -> Arc<TeamsAppState> {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        Arc::new(TeamsAppState::new(
            Arc::new(crate::service::DefaultTeamService::new(db.clone())),
            Arc::new(NoAudit),
            Arc::new(crate::checker::TeamProjectAccessChecker::new(db)),
            Arc::new(AllowSensitive),
        ))
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".into(),
            user_agent: "test".into(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".into(),
            scheme: "http".into(),
            host: "localhost".into(),
            is_secure: false,
        }
    }

    /// A deployment token never reaches another project's grants. Today the
    /// permission check refuses it first (deployment tokens do not map to
    /// `ProjectsRead`/`ProjectsWrite`); the project scope guard behind it
    /// keeps the refusal if that mapping ever changes.
    #[tokio::test]
    async fn a_deployment_token_cannot_touch_another_projects_grants() {
        use temps_entities::TeamRole;
        let token = || {
            AuthContext::new_deployment_token(
                1,
                None,
                None,
                9,
                "deploy".into(),
                vec![temps_entities::deployment_tokens::DeploymentTokenPermission::FullAccess],
            )
        };
        let other = 2;
        let statuses = [
            list_project_access(RequireAuth(token()), State(state()), Path(other))
                .await
                .map(|_| ())
                .expect_err("list"),
            grant_project_access(
                RequireAuth(token()),
                State(state()),
                Path(other),
                Extension(metadata()),
                Json(CreateProjectAccessRequest {
                    team_id: 3,
                    role: TeamRole::Viewer,
                }),
            )
            .await
            .map(|_| ())
            .expect_err("grant"),
            get_project_access(RequireAuth(token()), State(state()), Path((other, 3)))
                .await
                .map(|_| ())
                .expect_err("get"),
            update_project_access(
                RequireAuth(token()),
                State(state()),
                Path((other, 3)),
                Extension(metadata()),
                Json(UpdateProjectAccessRequest {
                    role: TeamRole::Viewer,
                }),
            )
            .await
            .map(|_| ())
            .expect_err("update"),
            revoke_project_access(
                RequireAuth(token()),
                State(state()),
                Path((other, 3)),
                Extension(metadata()),
            )
            .await
            .map(|_| ())
            .expect_err("revoke"),
        ];
        for problem in statuses {
            assert_eq!(problem.status_code, axum::http::StatusCode::FORBIDDEN);
            let kind = problem.body.get("type").and_then(|v| v.as_str());
            assert!(
                matches!(
                    kind,
                    Some(
                        "https://temps.sh/probs/insufficient-permissions"
                            | "https://temps.sh/probs/cross-project-access-denied"
                    )
                ),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn ordinary_users_are_subject_to_the_project_scoped_rules() {
        let auth = AuthContext::new_session(user(2), Role::User);
        assert!(!grant_authz(&auth).is_instance_admin);
    }
}
