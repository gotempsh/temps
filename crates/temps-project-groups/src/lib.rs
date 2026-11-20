// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project groups: an optional parent that gathers several projects (a
//! frontend, an API, a worker, …) into one unit.
//!
//! # Additive by design
//!
//! Grouping never changes how a project deploys or who can reach it. A
//! project with no membership row is ungrouped and behaves exactly as it did
//! before this crate existed; deleting a group only ungroups its projects.
//!
//! # Access
//!
//! Group endpoints use the instance-wide `Projects*` permissions. Member
//! lists are filtered through the same [`temps_core::ProjectAccessChecker`]
//! the project list uses, so a group never reveals a project the caller could
//! not open. Renaming or deleting a group requires access to every project in
//! it, and moving a project in or out requires write access to that project.

pub mod env_vars;
pub mod error;
pub mod handlers;
pub mod instantiation;
pub mod instantiation_handlers;
pub mod plugin;
pub mod repository;
pub mod routes;
pub mod service;

pub use env_vars::{inherited_env_vars, select_inherited, GroupEnvVarService};
pub use error::ProjectGroupError;
pub use handlers::{ProjectGroupsApiDoc, ProjectGroupsAppState};
pub use plugin::ProjectGroupsPlugin;
pub use repository::{
    GroupRepository, GroupRepositoryService, MemberSyncResult, RepositorySource, PUBLIC_PROVIDERS,
};
pub use routes::GroupRouteService;
pub use service::{
    CreateProjectGroupRequest, ProjectGroupService, ProjectGroupWithMembers,
    UpdateProjectGroupRequest,
};
