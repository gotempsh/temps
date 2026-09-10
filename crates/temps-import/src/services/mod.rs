// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Import orchestration services

pub mod deployment_verifier;
mod orchestrator;
pub mod resource_executor;

pub use deployment_verifier::DeploymentVerifier;
pub use orchestrator::ImportOrchestrator;
pub use resource_executor::ResourceExecutor;

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

    /// SSRF guard rejected a user-supplied importer `base_url` (Fix #16).
    #[error("Invalid importer base URL '{url}': {reason}")]
    InvalidBaseUrl { url: String, reason: String },
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
            ImportServiceError::Import(e) => import_error_to_problem(e),
            ImportServiceError::Internal(msg) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(msg)
            }
            ImportServiceError::InvalidBaseUrl { url, reason } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Importer Base URL")
                    .with_detail(format!("base_url '{url}' rejected: {reason}"))
            }
        }
    }
}

/// Map each `ImportError` variant to its own HTTP status — collapsing them
/// all into one status (e.g. always 400) would hide the difference between
/// "your credentials are wrong" (401), "the source platform is rate
/// limiting you" (429), and "temps couldn't reach it" (502), all of which
/// need different handling on the client.
fn import_error_to_problem(error: temps_import_types::ImportError) -> Problem {
    use temps_import_types::ImportError;
    match error {
        ImportError::AuthenticationError(_) | ImportError::InvalidCredentials(_) => {
            problemdetails::new(StatusCode::UNAUTHORIZED)
                .with_title("Source Authentication Failed")
                .with_detail(error.to_string())
        }
        ImportError::RateLimitExceeded(_) => problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
            .with_title("Source Rate Limit Exceeded")
            .with_detail(error.to_string()),
        ImportError::ContainerNotFound(_) => problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Workload Not Found")
            .with_detail(error.to_string()),
        ImportError::SourceNotAccessible(_)
        | ImportError::DiscoveryFailed(_)
        | ImportError::InspectionFailed(_)
        | ImportError::NetworkError(_) => problemdetails::new(StatusCode::BAD_GATEWAY)
            .with_title("Source Platform Unreachable")
            .with_detail(error.to_string()),
        ImportError::InvalidConfiguration(_)
        | ImportError::ValidationFailed(_)
        | ImportError::PlanGenerationFailed(_)
        | ImportError::UnsupportedFeature(_) => problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Import Request")
            .with_detail(error.to_string()),
        ImportError::ExecutionFailed(_)
        | ImportError::StepFailed { .. }
        | ImportError::VolumeError(_)
        | ImportError::ServiceMigrationError(_)
        | ImportError::DomainMigrationError(_)
        | ImportError::Internal(_)
        | ImportError::SerializationError(_) => {
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Import Execution Failed")
                .with_detail(error.to_string())
        }
    }
}
