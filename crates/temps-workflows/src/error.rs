// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The workflows domain error.
//!
//! No variant ever carries a payload (input, output, step result, event
//! payload or error message written by the app): only IDs, names, sizes and
//! the operation. That keeps payloads out of logs and Problem responses by
//! construction (ADR-103 §5); `payload_never_reaches_an_error_message` in
//! `payload.rs` asserts it.

use sea_orm::DbErr;
use temps_core::{EncryptionError, ExecutorEndpointsError, PayloadBlobError};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("Environment {environment_id} was not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },

    #[error("Workflow run {run_id} was not found in environment {environment_id} of project {project_id}")]
    RunNotFound {
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
    },

    #[error("Step {sequence} of workflow run {run_id} was not found")]
    StepNotFound { run_id: Uuid, sequence: i32 },

    #[error("Invalid {field}: {reason}")]
    Validation { field: &'static str, reason: String },

    #[error(
        "The {field} payload is {size} bytes, over the {limit}-byte limit{}",
        if *blob_unavailable { " (blob storage is not available, so the inline limit applies)" } else { "" }
    )]
    PayloadTooLarge {
        field: &'static str,
        size: usize,
        limit: usize,
        blob_unavailable: bool,
    },

    #[error("Quota '{quota}' of environment {environment_id} is reached ({limit}); new runs are blocked until it frees up")]
    QuotaExceeded {
        environment_id: i32,
        quota: &'static str,
        limit: i64,
    },

    #[error("Workflow run {run_id} is {status}; cannot {operation}")]
    InvalidState {
        run_id: Uuid,
        status: String,
        operation: &'static str,
    },

    #[error("Workflow run {run_id} rejected a write from lease {presented}: the current lease is {current} ({status})")]
    StaleLease {
        run_id: Uuid,
        presented: i64,
        current: i64,
        status: String,
    },

    #[error("WorkflowIncompatible {{ run_id: {run_id}, step: {sequence}, expected: {expected:?}, found: {found:?} }}")]
    Incompatible {
        run_id: Uuid,
        sequence: i32,
        expected: String,
        found: String,
    },

    #[error("Invocation token rejected: {reason}")]
    InvalidToken { reason: &'static str },

    #[error("Workflow credential rejected: {reason}")]
    InvalidCredential { reason: &'static str },

    #[error("Run {run_id} was not started by workflow credential {credential_id}")]
    NotStartedByCredential { run_id: Uuid, credential_id: i32 },

    #[error("Could not {operation} the {field} payload of workflow run {run_id}: {source}")]
    Encryption {
        run_id: Uuid,
        field: String,
        operation: &'static str,
        #[source]
        source: EncryptionError,
    },

    #[error("Could not encode the {field} payload of workflow run {run_id}: {reason}")]
    Encoding {
        run_id: Uuid,
        field: String,
        reason: String,
    },

    #[error(transparent)]
    Blob(#[from] PayloadBlobError),

    #[error(transparent)]
    Endpoints(#[from] ExecutorEndpointsError),

    #[error("Invoking workflow run {run_id} failed: {reason}")]
    Invocation { run_id: Uuid, reason: String },

    #[error("Database error while {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

impl WorkflowError {
    /// Map a `DbErr` raised while `operation`: a unique violation is a
    /// conflict the caller reports as validation, the rest stays a database
    /// error with the operation named.
    pub fn db(operation: &'static str) -> impl FnOnce(DbErr) -> WorkflowError {
        move |source| match source.sql_err() {
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => WorkflowError::Validation {
                field: "unique key",
                reason: format!("a conflicting row already exists while {operation}"),
            },
            _ => WorkflowError::Database { operation, source },
        }
    }
}

pub type WorkflowResult<T> = Result<T, WorkflowError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_reads_like_the_adr_names_it() {
        let run_id = Uuid::nil();
        let message = WorkflowError::Incompatible {
            run_id,
            sequence: 2,
            expected: "charge".into(),
            found: "refund".into(),
        }
        .to_string();
        assert_eq!(
            message,
            "WorkflowIncompatible { run_id: 00000000-0000-0000-0000-000000000000, step: 2, \
             expected: \"charge\", found: \"refund\" }"
        );
    }

    #[test]
    fn oversize_error_says_why_the_inline_limit_applies() {
        let error = WorkflowError::PayloadTooLarge {
            field: "input",
            size: 300_000,
            limit: 262_144,
            blob_unavailable: true,
        };
        assert!(error.to_string().contains("blob storage is not available"));
    }

    #[test]
    fn database_errors_name_the_operation() {
        let error = WorkflowError::db("claiming due runs")(DbErr::Custom("boom".into()));
        assert!(error.to_string().contains("claiming due runs"));
    }
}
