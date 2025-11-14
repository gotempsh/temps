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
}

pub type Result<T> = std::result::Result<T, DataError>;
