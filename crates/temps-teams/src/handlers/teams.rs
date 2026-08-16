use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::permission_guard;
use temps_auth::permissions::Role;
use temps_auth::RequireAuth;
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use temps_entities::teams;
use utoipa::ToSchema;

use crate::service::{
    CreateTeamMemberRequest, CreateTeamRequest, GrantAuthz, ProjectAccessResponse,
    TeamMemberResponse, UpdateMemberRoleRequest, UpdateTeamRequest,
};

use super::TeamsAppState;

// ---------------------------------------------------------------------------
// Audit event types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct TeamCreatedAudit {
    #[serde(flatten)]
    context: AuditContext,
    team_id: i32,
    slug: String,
}

impl AuditOperation for TeamCreatedAudit {
    fn operation_type(&self) -> String {
        "TEAM_CREATED".to_string()
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
            .map_err(|e| anyhow::anyhow!("failed to serialize TeamCreatedAudit: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
struct TeamUpdatedAudit {
    #[serde(flatten)]
    context: AuditContext,
    team_id: i32,
}

impl AuditOperation for TeamUpdatedAudit {
    fn operation_type(&self) -> String {
        "TEAM_UPDATED".to_string()
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
            .map_err(|e| anyhow::anyhow!("failed to serialize TeamUpdatedAudit: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
struct TeamDeletedAudit {
    #[serde(flatten)]
    context: AuditContext,
    team_id: i32,
}

impl AuditOperation for TeamDeletedAudit {
    fn operation_type(&self) -> String {
        "TEAM_DELETED".to_string()
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
            .map_err(|e| anyhow::anyhow!("failed to serialize TeamDeletedAudit: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
struct TeamMemberAddedAudit {
    #[serde(flatten)]
    context: AuditContext,
    team_id: i32,
    target_user_id: i32,
    role: String,
}

impl AuditOperation for TeamMemberAddedAudit {
    fn operation_type(&self) -> String {
        "TEAM_MEMBER_ADDED".to_string()
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
            .map_err(|e| anyhow::anyhow!("failed to serialize TeamMemberAddedAudit: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
struct TeamMemberRemovedAudit {
    #[serde(flatten)]
    context: AuditContext,
    team_id: i32,
    target_user_id: i32,
}

impl AuditOperation for TeamMemberRemovedAudit {
    fn operation_type(&self) -> String {
        "TEAM_MEMBER_REMOVED".to_string()
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
            .map_err(|e| anyhow::anyhow!("failed to serialize TeamMemberRemovedAudit: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
struct TeamMemberRoleUpdatedAudit {
    #[serde(flatten)]
    context: AuditContext,
    team_id: i32,
    target_user_id: i32,
    role: String,
}

impl AuditOperation for TeamMemberRoleUpdatedAudit {
    fn operation_type(&self) -> String {
        "TEAM_MEMBER_ROLE_UPDATED".to_string()
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
            .map_err(|e| anyhow::anyhow!("failed to serialize TeamMemberRoleUpdatedAudit: {e}"))
    }
}

pub(crate) fn router() -> Router<Arc<TeamsAppState>> {
    Router::new()
        .route("/teams", get(list_teams).post(create_team))
        .route(
            "/teams/{team_id}",
            get(get_team).patch(update_team).delete(delete_team),
        )
        .route(
            "/teams/{team_id}/members",
            get(list_team_members).post(add_team_member),
        )
        .route(
            "/teams/{team_id}/members/{user_id}",
            patch(update_team_member_role).delete(remove_team_member),
        )
        .route("/teams/{team_id}/projects", get(list_team_projects))
}

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TeamResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub created_by: i32,
    #[schema(example = "2026-07-30T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(example = "2026-07-30T12:15:47.609192Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<teams::Model> for TeamResponse {
    fn from(m: teams::Model) -> Self {
        Self {
            id: m.id,
            name: m.name,
            slug: m.slug,
            description: m.description,
            created_by: m.created_by,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TeamListResponse {
    pub teams: Vec<TeamResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListTeamsQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[utoipa::path(
    tag = "Teams",
    get,
    path = "/teams",
    params(
        ("page" = Option<u64>, Query, description = "1-indexed page"),
        ("page_size" = Option<u64>, Query, description = "default 20, max 100"),
    ),
    responses(
        (status = 200, description = "Paginated teams", body = TeamListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_teams(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Query(q): Query<ListTeamsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersRead);
    let (items, total) = state.team_service.list_teams(q.page, q.page_size).await?;
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q
        .page_size
        .unwrap_or(crate::service::DEFAULT_PAGE_SIZE)
        .clamp(1, crate::service::MAX_PAGE_SIZE);
    Ok(Json(TeamListResponse {
        teams: items.into_iter().map(TeamResponse::from).collect(),
        total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    tag = "Teams",
    post,
    path = "/teams",
    request_body = CreateTeamRequest,
    responses(
        (status = 201, description = "Team created", body = TeamResponse),
        (status = 400, description = "Validation error"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Slug already taken"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_team(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<CreateTeamRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);
    let team = state.team_service.create_team(auth.user_id(), req).await?;

    let audit = TeamCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        team_id: team.id,
        slug: team.slug.clone(),
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write team create audit log");
    }

    Ok((StatusCode::CREATED, Json(TeamResponse::from(team))))
}

#[utoipa::path(
    tag = "Teams",
    get,
    path = "/teams/{team_id}",
    params(("team_id" = i32, Path, description = "Team id")),
    responses(
        (status = 200, description = "Team", body = TeamResponse),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Team not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_team(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(team_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersRead);
    let team = state.team_service.get_team(team_id).await?;
    Ok(Json(TeamResponse::from(team)))
}

#[utoipa::path(
    tag = "Teams",
    patch,
    path = "/teams/{team_id}",
    params(("team_id" = i32, Path, description = "Team id")),
    request_body = UpdateTeamRequest,
    responses(
        (status = 200, description = "Updated team", body = TeamResponse),
        (status = 400, description = "Validation error"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Team not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_team(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(team_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<UpdateTeamRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);
    let team = state.team_service.update_team(team_id, req).await?;

    let audit = TeamUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        team_id,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write team update audit log");
    }

    Ok(Json(TeamResponse::from(team)))
}

#[utoipa::path(
    tag = "Teams",
    delete,
    path = "/teams/{team_id}",
    params(("team_id" = i32, Path, description = "Team id")),
    responses(
        (status = 204, description = "Team deleted"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Team not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_team(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(team_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersDelete);
    // Deleting a team cascades away its project grants. The service refuses
    // when that would strip a project's last grant and silently re-open it,
    // unless the caller is an instance admin — the same bar `revoke` sets.
    let authz = if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        GrantAuthz::instance_admin()
    } else {
        GrantAuthz::project_scoped()
    };
    let cascaded = state.team_service.delete_team(team_id, &authz).await?;
    let audit = TeamDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        team_id,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write team delete audit log");
    }
    // Without these, the trail shows only TEAM_DELETED and never says which
    // projects stopped being reachable through it.
    for grant in cascaded {
        let revoked = crate::handlers::project_access::ProjectAccessRevokedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: grant.project_id,
            team_id: grant.team_id,
        };
        if let Err(e) = state.audit.create_audit_log(&revoked).await {
            tracing::error!(
                error = %e,
                project_id = grant.project_id,
                team_id = grant.team_id,
                "teams: failed to audit a grant removed by team deletion"
            );
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Teams",
    get,
    path = "/teams/{team_id}/members",
    params(("team_id" = i32, Path)),
    responses(
        (status = 200, description = "Members", body = [TeamMemberResponse]),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Team not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_team_members(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(team_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersRead);
    let members = state.team_service.list_members(team_id).await?;
    let responses: Result<Vec<_>, _> = members
        .into_iter()
        .map(TeamMemberResponse::from_team_member)
        .collect();
    Ok(Json(responses.map_err(Problem::from)?))
}

#[utoipa::path(
    tag = "Teams",
    post,
    path = "/teams/{team_id}/members",
    params(("team_id" = i32, Path)),
    request_body = CreateTeamMemberRequest,
    responses(
        (status = 201, description = "Member added", body = TeamMemberResponse),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Team not found"),
        (status = 409, description = "User already a member"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn add_team_member(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(team_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<CreateTeamMemberRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);
    // Capture audit fields before req is moved into the service call.
    let target_user_id = req.user_id;
    let role = req.role.to_string();
    let member = state
        .team_service
        .add_member(auth.user_id(), team_id, req)
        .await?;
    let audit = TeamMemberAddedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        team_id,
        target_user_id,
        role,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write team member added audit log");
    }
    let response = TeamMemberResponse::from_team_member(member).map_err(Problem::from)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    tag = "Teams",
    patch,
    path = "/teams/{team_id}/members/{user_id}",
    params(("team_id" = i32, Path), ("user_id" = i32, Path)),
    request_body = UpdateMemberRoleRequest,
    responses(
        (status = 200, description = "Updated membership", body = TeamMemberResponse),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Member not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_team_member_role(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path((team_id, user_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<UpdateMemberRoleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);

    // Capture the audit field before req is moved into the service call.
    let audit_role = req.role.to_string();

    let member = state
        .team_service
        .set_member_role(team_id, user_id, req)
        .await?;

    let audit = TeamMemberRoleUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        team_id,
        target_user_id: user_id,
        role: audit_role,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write team member role update audit log");
    }

    let response = TeamMemberResponse::from_team_member(member).map_err(Problem::from)?;
    Ok(Json(response))
}

#[utoipa::path(
    tag = "Teams",
    delete,
    path = "/teams/{team_id}/members/{user_id}",
    params(("team_id" = i32, Path), ("user_id" = i32, Path)),
    responses(
        (status = 204, description = "Member removed"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Member not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove_team_member(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path((team_id, user_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, UsersWrite);
    state.team_service.remove_member(team_id, user_id).await?;
    let audit = TeamMemberRemovedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        team_id,
        target_user_id: user_id,
    };
    if let Err(e) = state.audit.create_audit_log(&audit).await {
        tracing::error!(error = %e, "teams: failed to write team member removed audit log");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Teams",
    get,
    path = "/teams/{team_id}/projects",
    params(("team_id" = i32, Path)),
    responses(
        (status = 200, description = "Projects this team has access to", body = [ProjectAccessResponse]),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Team not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_team_projects(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TeamsAppState>>,
    Path(team_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    // `UsersRead`, not `ProjectsRead`, to match the rest of this crate.
    // `ProjectsRead` is held by `Role::User` and `Role::Reader`, and team
    // ids are small sequential integers — guarding on it would let any
    // low-privilege account walk the whole access-control matrix (which
    // project is gated, to which team, at what role), defeating the
    // project-list filtering this feature exists to provide.
    permission_guard!(auth, UsersRead);
    let grants = state.team_service.list_team_projects(team_id).await?;
    let responses: Result<Vec<_>, _> = grants
        .into_iter()
        .map(ProjectAccessResponse::from_model)
        .collect();
    Ok(Json(responses.map_err(Problem::from)?))
}
