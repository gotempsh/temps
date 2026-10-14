// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProjectGroupError {
    #[error("Project group {group_id} not found")]
    NotFound { group_id: i32 },

    #[error("Project group with slug '{slug}' not found")]
    SlugNotFound { slug: String },

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Project {project_id} is not in project group {group_id}")]
    NotAMember { group_id: i32, project_id: i32 },

    #[error("Project group slug '{slug}' is already taken")]
    SlugConflict { slug: String },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Variable {var_id} not found in project group {group_id}")]
    EnvVarNotFound { group_id: i32, var_id: i32 },

    #[error("Project group {group_id} already has variable '{key}' for {scope}")]
    EnvVarConflict {
        group_id: i32,
        key: String,
        scope: String,
    },

    #[error("Could not encrypt or decrypt variable '{key}' of project group {group_id}: {reason}")]
    Encryption {
        group_id: i32,
        key: String,
        reason: String,
    },

    #[error(
        "Project group {group_id} has no repository. Connect one to the project first, then its services can follow it"
    )]
    RepositoryNotSet { group_id: i32 },

    #[error("Git connection {connection_id} cannot be used: {reason}")]
    GitConnection { connection_id: i32, reason: String },

    #[error(
        "Repository {owner}/{name} is not available through git connection {connection_id}. Sync that connection's repositories, or pick a repository it can access"
    )]
    RepositoryUnavailable {
        connection_id: i32,
        owner: String,
        name: String,
    },

    #[error("Public repository {owner}/{name} on {provider} cannot be used: {reason}")]
    PublicRepository {
        provider: String,
        owner: String,
        name: String,
        reason: String,
    },

    #[error("Branch '{branch}' could not be found in {owner}/{name}: {reason}")]
    BranchNotFound {
        owner: String,
        name: String,
        branch: String,
        reason: String,
    },

    #[error(
        "{name} deploys from {source_type}, not from Git, so it cannot follow the project's repository"
    )]
    NotGitSourced {
        project_id: i32,
        name: String,
        source_type: String,
    },

    #[error("Could not point {name} at the project's repository: {source}")]
    ServiceGit {
        project_id: i32,
        name: String,
        #[source]
        source: temps_projects::services::types::ProjectError,
    },

    #[error(
        "Instantiation {instantiation_id} cannot be resumed: the variable values it needs for {steps} were removed when its retention period ended. Start a new instantiation instead"
    )]
    InstantiationSecretsPurged {
        instantiation_id: i32,
        /// Keys of the remaining steps that need the removed values.
        steps: String,
    },

    #[error("Route {route_id} not found in project group {group_id}")]
    RouteNotFound { group_id: i32, route_id: i32 },

    /// The routes a membership change would drop are not the ones the caller
    /// authorized: a route involving the project was created in between.
    #[error(
        "Route {route_id} in project group {group_id} involves the project being moved and was \
         created while this request was being authorized; retry the request"
    )]
    RouteSetChanged { group_id: i32, route_id: i32 },

    /// The project was not in the group the placement was computed against:
    /// another request moved it between the check and the write, so the
    /// routes that were authorized may not be the ones the move drops.
    #[error(
        "Project {project_id} changed project group while it was being placed in project group \
         {group_id}; retry the request"
    )]
    MembershipChanged { group_id: i32, project_id: i32 },

    /// Projects started following the group's repository after the caller's
    /// write access to its followers was checked, so the change would have
    /// rewritten (or detached) projects the caller was never authorized on.
    /// The ids are kept for logs; the message gives only the count, because
    /// the caller may not be allowed to see those projects.
    #[error(
        "{} project(s) started following the repository of project group {group_id} while \
         this request was being authorized; retry the request",
        .project_ids.len()
    )]
    RepositoryFollowersChanged {
        group_id: i32,
        project_ids: Vec<i32>,
    },

    /// Projects joined the group after the caller's access to its members was
    /// checked, so the change would have been made to (or have reached)
    /// projects the caller was never authorized on. Nothing was changed. The
    /// ids are kept for logs; the message gives only the count, because the
    /// caller may not be allowed to see those projects.
    #[error(
        "{} project(s) joined project group {group_id} while this request was being \
         authorized; retry the request",
        .project_ids.len()
    )]
    MembersChanged {
        group_id: i32,
        project_ids: Vec<i32>,
    },

    /// The group's repository was changed while a project was being pointed
    /// at the one it had when the request started, so the project would have
    /// been left following a repository the group no longer has.
    #[error(
        "The repository of project group {group_id} changed while project {project_id} was being \
         pointed at it; retry the request"
    )]
    RepositoryChanged { group_id: i32, project_id: i32 },

    /// Another request is changing (or clearing) the group's repository and
    /// writing it through to the members right now. Two such changes must not
    /// interleave their member writes, or a member could end up on the older
    /// repository while the group stores the newer one. Nothing was changed.
    #[error(
        "The repository of project group {group_id} is being changed by another request; \
         retry once it has finished"
    )]
    RepositoryChangeInProgress { group_id: i32 },

    /// Too many repository changes (across groups) are being written through
    /// at once on this server. Each holds a database connection for its whole
    /// duration; letting them all take one would leave the pool with none for
    /// the writes. Nothing was changed; retry shortly.
    #[error(
        "Too many repository changes are in progress ({limit} at once); the repository of \
         project group {group_id} was not changed, retry shortly"
    )]
    TooManyRepositoryChanges { group_id: i32, limit: usize },

    #[error("{source_name} already sends {path_prefix} to another service")]
    RouteConflict {
        source_name: String,
        path_prefix: String,
    },

    #[error("Database error while {operation}")]
    Database {
        operation: String,
        #[source]
        source: sea_orm::DbErr,
    },

    /// Internal logic error: something that should never happen in a
    /// correctly-built binary (e.g. a plan that cannot be serialized).
    #[error("Internal error: {message}")]
    Internal { message: String },
}

impl ProjectGroupError {
    pub(crate) fn db(operation: impl Into<String>) -> impl FnOnce(sea_orm::DbErr) -> Self {
        let operation = operation.into();
        move |source| ProjectGroupError::Database { operation, source }
    }
}

impl From<ProjectGroupError> for Problem {
    fn from(error: ProjectGroupError) -> Self {
        match error {
            ProjectGroupError::NotFound { .. }
            | ProjectGroupError::SlugNotFound { .. }
            | ProjectGroupError::ProjectNotFound { .. }
            | ProjectGroupError::NotAMember { .. }
            | ProjectGroupError::EnvVarNotFound { .. }
            | ProjectGroupError::RouteNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(error.to_string()),

            ProjectGroupError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),

            ProjectGroupError::GitConnection { .. }
            | ProjectGroupError::RepositoryUnavailable { .. }
            | ProjectGroupError::PublicRepository { .. }
            | ProjectGroupError::BranchNotFound { .. }
            | ProjectGroupError::NotGitSourced { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Repository Not Usable")
                    .with_detail(error.to_string())
            }

            ProjectGroupError::RepositoryNotSet { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Repository Not Connected")
                .with_detail(error.to_string()),

            ProjectGroupError::InstantiationSecretsPurged { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Instantiation Cannot Be Resumed")
                    .with_detail(error.to_string())
            }

            // Keep the status the service's own git endpoint would return
            // (e.g. 403 for the host-socket guard), with the service named.
            ProjectGroupError::ServiceGit { name, source, .. } => {
                let detail =
                    format!("Could not point {name} at the project's repository: {source}");
                Problem::from(source).with_detail(detail)
            }

            ProjectGroupError::SlugConflict { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Slug Already Taken")
                .with_detail(error.to_string()),

            ProjectGroupError::RouteConflict { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Route Already Exists")
                .with_detail(error.to_string()),

            ProjectGroupError::RouteSetChanged { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Routes Changed")
                .with_detail(error.to_string()),

            ProjectGroupError::MembershipChanged { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Membership Changed")
                    .with_detail(error.to_string())
            }

            ProjectGroupError::MembersChanged { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Group Members Changed")
                .with_detail(error.to_string()),

            ProjectGroupError::RepositoryFollowersChanged { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Repository Followers Changed")
                    .with_detail(error.to_string())
            }

            ProjectGroupError::RepositoryChanged { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Repository Changed")
                    .with_detail(error.to_string())
            }

            ProjectGroupError::RepositoryChangeInProgress { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Repository Change In Progress")
                    .with_detail(error.to_string())
            }

            ProjectGroupError::TooManyRepositoryChanges { .. } => {
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Too Many Repository Changes")
                    .with_detail(error.to_string())
            }

            ProjectGroupError::EnvVarConflict { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Variable Already Exists")
                .with_detail(error.to_string()),

            ProjectGroupError::Database {
                ref operation,
                ref source,
            } => {
                tracing::error!(operation = %operation, %source, "project groups: database failure");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("A database error occurred; check server logs for details")
            }

            ProjectGroupError::Encryption { .. } | ProjectGroupError::Internal { .. } => {
                tracing::error!(error = %error, "project groups: internal failure");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("An internal error occurred; check server logs for details")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_followers_are_a_conflict_that_counts_but_does_not_name_them() {
        let error = ProjectGroupError::RepositoryFollowersChanged {
            group_id: 7,
            project_ids: vec![41, 42],
        };
        let message = error.to_string();
        assert!(message.contains("2 project(s)"), "got {message}");
        assert!(message.contains("project group 7"), "got {message}");
        // The caller may not be able to see those projects.
        assert!(!message.contains("41") && !message.contains("42"));
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn members_that_joined_during_authorization_are_a_conflict_that_counts_but_does_not_name_them()
    {
        let error = ProjectGroupError::MembersChanged {
            group_id: 7,
            project_ids: vec![41, 42],
        };
        let message = error.to_string();
        assert!(message.contains("2 project(s)"), "got {message}");
        assert!(message.contains("project group 7"), "got {message}");
        assert!(!message.contains("41") && !message.contains("42"));
        assert_eq!(Problem::from(error).status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn a_repository_changed_under_a_project_is_a_conflict_naming_both_ids() {
        let error = ProjectGroupError::RepositoryChanged {
            group_id: 7,
            project_id: 41,
        };
        let message = error.to_string();
        assert!(
            message.contains("project group 7") && message.contains("project 41"),
            "got {message}"
        );
        assert_eq!(Problem::from(error).status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn a_concurrent_repository_change_is_a_conflict_naming_the_group() {
        let error = ProjectGroupError::RepositoryChangeInProgress { group_id: 7 };
        let message = error.to_string();
        assert!(message.contains("project group 7"), "got {message}");
        assert_eq!(Problem::from(error).status_code, StatusCode::CONFLICT);
    }

    #[test]
    fn too_many_repository_changes_is_a_retryable_429_naming_the_group_and_limit() {
        let error = ProjectGroupError::TooManyRepositoryChanges {
            group_id: 7,
            limit: 2,
        };
        let message = error.to_string();
        assert!(
            message.contains("project group 7") && message.contains("2 at once"),
            "got {message}"
        );
        assert_eq!(
            Problem::from(error).status_code,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
}
