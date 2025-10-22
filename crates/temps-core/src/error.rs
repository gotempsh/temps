//! Common error types used across all Temps services

use thiserror::Error;

/// Common service error types
#[derive(Error, Debug)]
pub enum ServiceError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Not found: {resource}")]
    NotFound { resource: String },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Permission denied: {action}")]
    PermissionDenied { action: String },

    #[error("Configuration error: {message}")]
    Configuration { message: String },

    #[error("External service error: {service} - {message}")]
    ExternalService { service: String, message: String },

    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

/// Result type alias for service operations
pub type ServiceResult<T> = Result<T, ServiceError>;
