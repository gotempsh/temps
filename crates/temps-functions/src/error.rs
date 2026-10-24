// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed errors of the functions product.

use temps_workload_activator::ActivationError;
use temps_workload_plane::PlaneError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FunctionError {
    #[error("Function {function_id} not found in project {project_id}")]
    NotFound { project_id: i32, function_id: i64 },

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Project {project_id} already has a function named '{name}'")]
    DuplicateName { project_id: i32, name: String },

    /// Functions run on the workload plane, and this process has none.
    #[error("Functions are unavailable: {reason}")]
    PlaneUnavailable { reason: String },

    /// No runtime on the control plane runs this kind of artifact.
    #[error(
        "No runtime in this installation runs {artifact_kind} functions (control-plane \
         runtimes: {available}); {hint}"
    )]
    NoRuntime {
        artifact_kind: &'static str,
        available: String,
        hint: &'static str,
    },

    /// The plane refused or failed an operation on the function's workload.
    #[error(
        "Function '{name}' (id {function_id}): the workload plane could not {operation}: {source}"
    )]
    Plane {
        function_id: i64,
        name: String,
        operation: &'static str,
        /// Boxed: the plane's error is large and this enum is returned
        /// everywhere.
        #[source]
        source: Box<PlaneError>,
    },

    /// The function could not be started, or its instance stopped.
    #[error("Function '{name}' (id {function_id}): {source}")]
    Activation {
        function_id: i64,
        name: String,
        #[source]
        source: ActivationError,
    },

    /// Project deletion could not retire every function of the project.
    #[error(
        "Retiring the functions of project {project_id} failed for {failed} of {total}: {reasons}"
    )]
    ProjectFunctionsNotRetired {
        project_id: i32,
        failed: usize,
        total: usize,
        reasons: String,
    },

    #[error("Function artifact storage failed at {path}: {reason}")]
    Storage { path: String, reason: String },

    #[error("Database error while {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl From<sea_orm::DbErr> for FunctionError {
    fn from(source: sea_orm::DbErr) -> Self {
        FunctionError::Database {
            operation: "accessing functions",
            source,
        }
    }
}

impl FunctionError {
    /// A `map_err` adapter naming the operation.
    pub(crate) fn db(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| FunctionError::Database { operation, source }
    }
}
