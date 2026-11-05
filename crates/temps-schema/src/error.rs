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
    #[error(transparent)]
    LockSlotsExhausted(Box<LockSlotsExhausted>),

    #[error(transparent)]
    Service(#[from] ServiceError),
}

impl From<LockSlotsExhausted> for SchemaError {
    fn from(error: LockSlotsExhausted) -> Self {
        Self::LockSlotsExhausted(Box::new(error))
    }
}

impl From<SchemaError> for ServiceError {
    fn from(error: SchemaError) -> Self {
        match error {
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
