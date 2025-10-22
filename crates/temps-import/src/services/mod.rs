//! Import orchestration services

mod orchestrator;

pub use orchestrator::ImportOrchestrator;

use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
use thiserror::Error;

/// Import service errors
#[derive(Error, Debug)]
pub enum ImportServiceError {
    #[error("Import error: {0}")]
    Import(#[from] temps_import_types::ImportError),

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Source not available: {0}")]
    SourceNotAvailable(String),

    #[error("Validation failed")]
    ValidationFailed,

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type for import services
pub type ImportServiceResult<T> = Result<T, ImportServiceError>;

impl From<ImportServiceError> for Problem {
    fn from(error: ImportServiceError) -> Self {
        match error {
            ImportServiceError::SessionNotFound(msg) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Session Not Found")
                .with_detail(msg),
            ImportServiceError::SourceNotAvailable(msg) => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Source Not Available")
                    .with_detail(msg)
            }
            ImportServiceError::ValidationFailed => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Failed")
                .with_detail("Import validation failed"),
            ImportServiceError::Validation(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(msg),
            ImportServiceError::Configuration(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Configuration Error")
                    .with_detail(msg)
            }
            ImportServiceError::ExecutionFailed(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Execution Failed")
                    .with_detail(msg)
            }
            ImportServiceError::Database(e) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(e.to_string())
            }
            ImportServiceError::Import(e) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Import Error")
                .with_detail(e.to_string()),
            ImportServiceError::Internal(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(msg)
            }
        }
    }
}
