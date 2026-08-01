//! Teams and project-scoped role-based access control.
//!
//! # What this gives you
//!
//! - **Teams** — named groups of users (`/teams`).
//! - **Project access** — which teams may reach which projects, and with
//!   what role (`/projects/{project_id}/access`).
//!
//! # How enforcement works
//!
//! The platform already threads two guards through every project-scoped
//! handler:
//!
//! - `project_access_guard!` — given the auth context, a project id, and
//!   a checker: "may this user touch this project at all?"
//! - `project_permission_guard!` — the same plus a specific `Permission`:
//!   "may they perform *this action* in it?"
//!
//! Both resolve through the [`temps_core::ProjectAccessChecker`] trait and
//! are no-ops until something implements it.
//! [`TeamProjectAccessChecker`] is that implementation, registered by
//! [`TeamsPlugin`].
//!
//! # The safety property that makes this shippable
//!
//! A project with **zero** rows in `project_team_access` is unrestricted —
//! it behaves exactly as it did before this crate existed. Team gating
//! begins on a project the moment its first grant is added, and nowhere
//! else. An operator can create teams, add members, and look around
//! without locking anybody out of anything by accident.
//!
//! Instance admins are never restricted by team membership — that bypass
//! lives in the guard macro, not here.
//!
//! # Richer roles than the fixed four
//!
//! A plugin can override what a single membership resolves to by
//! registering a [`temps_core::MembershipPermissionResolver`]. Its answer
//! is intersected with the team's grant on the project, so it can only
//! narrow a member below their team's access, never widen them past it.

pub mod checker;
pub mod error;
pub mod handlers;
pub mod plugin;
pub mod role_permissions;
pub mod service;

pub use checker::TeamProjectAccessChecker;
pub use error::TeamError;
pub use handlers::{TeamsApiDoc, TeamsAppState};
pub use plugin::TeamsPlugin;
pub use role_permissions::fixed_role_permissions;
pub use service::{
    CreateProjectAccessRequest, CreateTeamMemberRequest, CreateTeamRequest, DefaultTeamService,
    ProjectAccessResponse, TeamMember, TeamMemberResponse, TeamService, UpdateMemberRoleRequest,
    UpdateTeamRequest,
};
