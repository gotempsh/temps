// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

/// What can go wrong reading or changing internal access grants (ADR 100).
#[derive(Debug, Error)]
pub enum InternalAccessError {
    #[error("Project {project_id} not found, or it is being deleted")]
    ProjectNotFound { project_id: i32 },

    #[error(
        "Environment {environment_id} not found in project {project_id}, or it has been deleted"
    )]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },

    #[error("Internal access grant {grant_id} not found on project {project_id}")]
    GrantNotFound { project_id: i32, grant_id: i32 },

    #[error(
        "Project {project_id} cannot grant internal access to itself: its own environments can \
         already call each other"
    )]
    SelfGrant { project_id: i32 },

    #[error(
        "Project {target_project_id} already grants this scope to project {caller_project_id}"
    )]
    DuplicateGrant {
        target_project_id: i32,
        caller_project_id: i32,
    },

    #[error(
        "Project {project_id} already has {limit} internal access grants, the most it may have; \
         remove one, or replace several environment grants with one for the whole project"
    )]
    TooManyGrants { project_id: i32, limit: u64 },

    #[error("Grant description is {length} characters long; the most allowed is {max}")]
    DescriptionTooLong { length: usize, max: usize },

    #[error("Internal access database operation '{operation}' failed: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl InternalAccessError {
    pub(crate) fn db(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| InternalAccessError::Database { operation, source }
    }
}
