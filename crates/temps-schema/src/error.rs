// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors from applying migrations and post-migration maintenance.

use crate::lock_capacity::LockSlotsExhausted;
use temps_core::ServiceError;
use thiserror::Error;

/// Why applying migrations or running post-migration maintenance failed.
///
/// [`Self::LockSlotsExhausted`] is kept apart because its fix is on the
/// PostgreSQL server (a setting and a restart), not in Temps or the data.
#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("Failed to validate access vault constraint '{constraint}' on '{table}': {reason}")]
    VaultConstraint {
        constraint: &'static str,
        table: &'static str,
        reason: String,
    },
    #[error("Failed to {operation} access vault index '{index}': {reason}")]
    VaultIndex {
        index: &'static str,
        operation: &'static str,
        reason: String,
    },
    #[error(transparent)]
    LockSlotsExhausted(Box<LockSlotsExhausted>),

    #[error(transparent)]
    Service(#[from] ServiceError),
}

impl From<LockSlotsExhausted> for SchemaError {
    /// Box the lock exhaustion, which is large next to the other variant.
    fn from(error: LockSlotsExhausted) -> Self {
        Self::LockSlotsExhausted(Box::new(error))
    }
}

impl From<SchemaError> for ServiceError {
    /// For callers that report a [`ServiceError`]: a lock exhaustion becomes
    /// [`ServiceError::Configuration`] with the full advice as its message.
    fn from(error: SchemaError) -> Self {
        match error {
            error @ (SchemaError::VaultIndex { .. } | SchemaError::VaultConstraint { .. }) => {
                ServiceError::Database(error.to_string())
            }
            // A server setting the operator has to change.
            SchemaError::LockSlotsExhausted(error) => ServiceError::Configuration {
                message: error.to_string(),
            },
            SchemaError::Service(error) => error,
        }
    }
}

/// Result of a migration or maintenance operation.
pub type SchemaResult<T> = Result<T, SchemaError>;
