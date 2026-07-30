//! Axum handlers for teams, project access, and custom roles.
//!
//! Managing teams and grants is an instance-administration surface: the
//! guards here are the instance-wide `Users*`/`Projects*` permissions, not
//! the project-scoped team roles this crate defines. Those roles govern
//! what a member may do *inside* a project they've been granted, and are
//! enforced by `project_access_guard!`/`project_permission_guard!` in the
//! project-scoped handlers across the rest of the platform.

mod custom_roles;
mod project_access;
mod teams;

pub use custom_roles::*;
pub use project_access::*;
pub use teams::*;

use std::sync::Arc;

use axum::Router;
use temps_core::AuditLogger;
use utoipa::OpenApi;

use crate::checker::TeamProjectAccessChecker;
use crate::custom_role_service::{
    CreateCustomRoleRequest, CustomRoleListResponse, CustomRoleResponse, CustomRoleService,
    UpdateCustomRoleRequest,
};
use crate::service::{
    CreateProjectAccessRequest, CreateTeamMemberRequest, CreateTeamRequest, ProjectAccessResponse,
    TeamMemberResponse, TeamService, UpdateMemberRoleRequest, UpdateTeamRequest,
};

/// Handler state shared across all teams routes.
#[derive(Clone)]
pub struct TeamsAppState {
    pub team_service: Arc<dyn TeamService>,
    pub custom_role_service: Arc<dyn CustomRoleService>,
    /// Records authorization-state changes (team delete, membership,
    /// project-access grant/revoke, custom-role edits) to the audit log.
    pub audit: Arc<dyn AuditLogger>,
    /// Held as the concrete type (not the `Arc<dyn ProjectAccessChecker>`
    /// trait object the rest of the platform sees) so the custom-role
    /// update/delete handlers can call `invalidate_all()` after a
    /// permission-set change — that changes what's cached for every member
    /// holding the role without any membership row changing, so the
    /// service's narrower `invalidate_user`/`invalidate_project` calls
    /// don't cover it.
    pub checker: Arc<TeamProjectAccessChecker>,
}

impl TeamsAppState {
    pub fn new(
        team_service: Arc<dyn TeamService>,
        custom_role_service: Arc<dyn CustomRoleService>,
        audit: Arc<dyn AuditLogger>,
        checker: Arc<TeamProjectAccessChecker>,
    ) -> Self {
        Self {
            team_service,
            custom_role_service,
            audit,
            checker,
        }
    }
}

pub fn router(state: Arc<TeamsAppState>) -> Router {
    Router::new()
        .merge(teams::router())
        .merge(project_access::router())
        .merge(custom_roles::router())
        .with_state(state)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        teams::list_teams,
        teams::create_team,
        teams::get_team,
        teams::update_team,
        teams::delete_team,
        teams::list_team_members,
        teams::add_team_member,
        teams::update_team_member_role,
        teams::remove_team_member,
        teams::list_team_projects,
        project_access::list_project_access,
        project_access::grant_project_access,
        project_access::revoke_project_access,
        custom_roles::list_custom_roles,
        custom_roles::create_custom_role,
        custom_roles::get_custom_role,
        custom_roles::update_custom_role,
        custom_roles::delete_custom_role,
    ),
    components(schemas(
        teams::TeamResponse,
        teams::TeamListResponse,
        TeamMemberResponse,
        ProjectAccessResponse,
        CreateTeamRequest,
        UpdateTeamRequest,
        CreateTeamMemberRequest,
        CreateProjectAccessRequest,
        UpdateMemberRoleRequest,
        CustomRoleResponse,
        CustomRoleListResponse,
        CreateCustomRoleRequest,
        UpdateCustomRoleRequest,
        temps_entities::TeamRole,
    )),
    tags((name = "Teams", description = "Teams, project-scoped access, and custom roles"))
)]
pub struct TeamsApiDoc;
