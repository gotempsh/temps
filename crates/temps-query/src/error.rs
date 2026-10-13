// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

/// Unified error type for all data source operations
#[derive(Error, Debug)]
pub enum DataError {
    /// Connection failed (authentication, network, etc.)
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// Connection lost or closed unexpectedly
    #[error("Connection lost: {0}")]
    ConnectionLost(String),

    /// Query execution failed
    #[error("Query failed: {0}")]
    QueryFailed(String),

    /// Query timeout
    #[error("Query timeout after {0}ms")]
    QueryTimeout(u64),

    /// Invalid query syntax or parameters
    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    /// Schema/introspection error
    #[error("Schema error: {0}")]
    SchemaError(String),

    /// Entity or namespace not found
    #[error("Not found: {0}")]
    NotFound(String),

    /// Operation not supported by this backend
    #[error("Operation not supported: {0}")]
    OperationNotSupported(String),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// A backend result exceeded a server-defined memory/shape budget.
    #[error(
        "Query result for entity '{entity}' exceeded {limit_kind} limit {limit} \
         (observed at least {observed})"
    )]
    ResultLimitExceeded {
        entity: String,
        limit_kind: &'static str,
        limit: usize,
        observed: usize,
    },

    /// A backend query failed. The source error is logged separately and is
    /// intentionally absent here because database diagnostics may echo filter
    /// literals or raw SQL.
    #[error("{backend} query failed for entity '{entity}'")]
    BackendQueryFailed {
        backend: &'static str,
        entity: String,
    },

    /// A container path nests deeper than the backend's hierarchy allows.
    ///
    /// This is a client error (the caller asked for the child containers of a
    /// leaf container, e.g. the sub-containers of a PostgreSQL schema), not a
    /// backend failure, so HTTP layers must map it to a 4xx.
    #[error(
        "{backend} containers nest at most {max_depth} level(s) deep, but container path \
         '{path}' is {depth} level(s) deep; it has no child containers, list its entities instead"
    )]
    ContainerPathTooDeep {
        backend: &'static str,
        path: String,
        depth: usize,
        max_depth: usize,
    },

    /// Permission denied
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Invalid credentials
    #[error("Invalid credentials")]
    InvalidCredentials,

    /// Generic backend error
    #[error("Backend error: {0}")]
    BackendError(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl DataError {
    /// Create a "not found" error with custom message
    pub fn not_found(msg: impl Into<String>) -> Self {
        DataError::NotFound(msg.into())
    }

    /// Create an operation not supported error
    pub fn operation_not_supported(msg: impl Into<String>) -> Self {
        DataError::OperationNotSupported(msg.into())
    }

    /// Create an invalid configuration error
    pub fn invalid_configuration(msg: impl Into<String>) -> Self {
        DataError::InvalidConfiguration(msg.into())
    }

    /// Create a permission denied error
    pub fn permission_denied(msg: impl Into<String>) -> Self {
        DataError::PermissionDenied(msg.into())
    }

    /// Create a "container path too deep" error for `path` in `backend`'s
    /// hierarchy. `max_depth` is the number of container levels the backend
    /// has (PostgreSQL: 2, database/schema), so a path of that depth is a
    /// leaf container and has no child containers.
    pub fn container_path_too_deep(
        backend: &'static str,
        path: &crate::types::ContainerPath,
        max_depth: usize,
    ) -> Self {
        DataError::ContainerPathTooDeep {
            backend,
            path: path.segments.join("/"),
            depth: path.depth(),
            max_depth,
        }
    }
}

pub type Result<T> = std::result::Result<T, DataError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContainerPath;

    #[test]
    fn container_path_too_deep_reports_path_depth_and_limit() {
        let error = DataError::container_path_too_deep(
            "PostgreSQL",
            &ContainerPath::from_slice(&["appdb", "pg_catalog"]),
            2,
        );

        assert!(matches!(
            error,
            DataError::ContainerPathTooDeep {
                backend: "PostgreSQL",
                depth: 2,
                max_depth: 2,
                ..
            }
        ));
        let message = error.to_string();
        assert!(message.contains("PostgreSQL"), "{message}");
        assert!(message.contains("'appdb/pg_catalog'"), "{message}");
        assert!(message.contains("2 level(s) deep"), "{message}");
        assert!(message.contains("at most 2 level(s)"), "{message}");
    }
}
