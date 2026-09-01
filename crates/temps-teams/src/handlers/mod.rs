// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Axum handlers for teams, project access, and custom roles.
//!
//! Managing teams and grants is an instance-administration surface: the
//! guards here are the instance-wide `Users*`/`Projects*` permissions, not
//! the project-scoped team roles this crate defines. Those roles govern
//! what a member may do *inside* a project they've been granted, and are
//! enforced by `project_access_guard!`/`project_permission_guard!` in the
//! project-scoped handlers across the rest of the platform.

mod project_access;
mod teams;

pub use project_access::*;
pub use teams::*;

use std::sync::Arc;

use axum::Router;
use temps_core::AuditLogger;
use utoipa::OpenApi;

use crate::checker::TeamProjectAccessChecker;
use crate::service::{
    CreateProjectAccessRequest, CreateTeamMemberRequest, CreateTeamRequest, ProjectAccessResponse,
    TeamMemberResponse, TeamService, UpdateMemberRoleRequest, UpdateTeamRequest,
};

/// Handler state shared across all teams routes.
#[derive(Clone)]
pub struct TeamsAppState {
    pub team_service: Arc<dyn TeamService>,
    /// Records authorization-state changes (team delete, membership,
    /// project-access grant/revoke) to the audit log.
    pub audit: Arc<dyn AuditLogger>,
    /// Held as the concrete type, not the `Arc<dyn ProjectAccessChecker>`
    /// trait object the rest of the platform sees, so the access handlers
    /// can guard themselves with the very checker this crate registers.
    pub checker: Arc<TeamProjectAccessChecker>,
    /// Central policy evaluator for sensitive mutations (e.g. deleting a
    /// team) — challenges with MFA step-up when the acting user has one
    /// enrolled. See [`temps_core::SensitiveActionAuthorizer`].
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
}

impl TeamsAppState {
    pub fn new(
        team_service: Arc<dyn TeamService>,
        audit: Arc<dyn AuditLogger>,
        checker: Arc<TeamProjectAccessChecker>,
        sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
    ) -> Self {
        Self {
            team_service,
            audit,
            checker,
            sensitive_action_authorizer,
        }
    }
}

pub fn router(state: Arc<TeamsAppState>) -> Router {
    Router::new()
        .merge(teams::router())
        .merge(project_access::router())
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
        temps_entities::TeamRole,
    )),
    tags((name = "Teams", description = "Teams and project-scoped access"))
)]
pub struct TeamsApiDoc;
