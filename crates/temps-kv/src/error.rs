//! Error types for the KV service

use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
use thiserror::Error;

/// Errors that can occur in the KV service
#[derive(Error, Debug)]
pub enum KvError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Docker error: {0}")]
    Docker(String),

    #[error("Container not running")]
    ContainerNotRunning,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Invalid key pattern: {0}")]
    InvalidPattern(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(i32),

    #[error("Service not configured for project")]
    ServiceNotConfigured,

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<KvError> for Problem {
    fn from(error: KvError) -> Self {
        match error {
            KvError::Redis(e) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Redis Error")
                .with_detail(e.to_string()),

            KvError::Docker(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Docker Error")
                .with_detail(msg),

            KvError::ContainerNotRunning => problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("Service Unavailable")
                .with_detail("KV service container is not running"),

            KvError::ConnectionFailed(msg) => problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("Connection Failed")
                .with_detail(msg),

            KvError::Serialization(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Serialization Error")
                .with_detail(msg),

            KvError::KeyNotFound(key) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Key Not Found")
                .with_detail(format!("Key '{}' does not exist", key)),

            KvError::InvalidPattern(pattern) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Pattern")
                .with_detail(format!("Invalid key pattern: {}", pattern)),

            KvError::ProjectNotFound(id) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(format!("Project {} does not exist", id)),

            KvError::ServiceNotConfigured => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Service Not Configured")
                .with_detail("KV service is not configured for this project"),

            KvError::Internal(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Error")
                .with_detail(msg),
        }
    }
}
