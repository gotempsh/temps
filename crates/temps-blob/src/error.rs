//! Error types for the Blob service

use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
use thiserror::Error;

/// Errors that can occur in the Blob service
#[derive(Error, Debug)]
pub enum BlobError {
    #[error("S3 error: {0}")]
    S3(String),

    #[error("Docker error: {0}")]
    Docker(String),

    #[error("Container not running")]
    ContainerNotRunning,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Blob not found: {0}")]
    NotFound(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Upload failed: {0}")]
    UploadFailed(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(i32),

    #[error("Service not configured for project")]
    ServiceNotConfigured,

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<BlobError> for Problem {
    fn from(error: BlobError) -> Self {
        match error {
            BlobError::S3(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Storage Error")
                .with_detail(msg),

            BlobError::Docker(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Docker Error")
                .with_detail(msg),

            BlobError::ContainerNotRunning => problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("Service Unavailable")
                .with_detail("Blob storage container is not running"),

            BlobError::ConnectionFailed(msg) => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Connection Failed")
                    .with_detail(msg)
            }

            BlobError::NotFound(path) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Blob Not Found")
                .with_detail(format!("Blob '{}' does not exist", path)),

            BlobError::InvalidPath(path) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Path")
                .with_detail(format!("Invalid blob path: {}", path)),

            BlobError::UploadFailed(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Upload Failed")
                .with_detail(msg),

            BlobError::ProjectNotFound(id) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(format!("Project {} does not exist", id)),

            BlobError::ServiceNotConfigured => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Service Not Configured")
                .with_detail("Blob storage is not configured for this project"),

            BlobError::Internal(msg) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Error")
                .with_detail(msg),
        }
    }
}
