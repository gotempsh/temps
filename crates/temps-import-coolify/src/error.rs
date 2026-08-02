//! Error types for the Coolify importer

use temps_import_types::ImportError;
use thiserror::Error;

/// Errors specific to Coolify API access
#[derive(Error, Debug)]
pub enum CoolifyImportError {
    #[error("No Coolify base URL provided: pass the instance URL (e.g. http://1.2.3.4:8000) in credentials.base_url")]
    MissingBaseUrl,

    #[error("No Coolify API token provided: create one under Keys & Tokens → API tokens and pass it in credentials.token")]
    MissingToken,

    #[error("Coolify base URL '{url}' is not usable: {reason}")]
    InvalidBaseUrl { url: String, reason: String },

    #[error("Coolify API request '{operation}' failed: {reason}")]
    Http { operation: String, reason: String },

    #[error("Coolify rejected the API token during '{operation}' (HTTP 401) — the token may be revoked or belong to another instance")]
    Unauthorized { operation: String },

    #[error("The Coolify instance has its API disabled (HTTP 403 on '{operation}') — enable it under Settings → Advanced → API, or in Security → API tokens")]
    ApiDisabled { operation: String },

    #[error("Coolify API '{operation}' returned HTTP {status}: {body}")]
    Api {
        operation: String,
        status: u16,
        body: String,
    },

    #[error("Coolify API '{operation}' returned a response temps could not parse: {reason}")]
    UnexpectedResponse { operation: String, reason: String },

    #[error("No Coolify application or database with UUID '{uuid}' found on the instance")]
    WorkloadNotFound { uuid: String },

    #[error("Could not resolve the Coolify project for environment id {environment_id} — the workload may belong to a deleted project")]
    ProjectResolution { environment_id: i64 },
}

impl From<CoolifyImportError> for ImportError {
    fn from(error: CoolifyImportError) -> Self {
        match error {
            CoolifyImportError::MissingBaseUrl
            | CoolifyImportError::MissingToken
            | CoolifyImportError::InvalidBaseUrl { .. } => {
                ImportError::InvalidCredentials(error.to_string())
            }
            CoolifyImportError::Unauthorized { .. } | CoolifyImportError::ApiDisabled { .. } => {
                ImportError::InvalidCredentials(error.to_string())
            }
            CoolifyImportError::Http { .. } | CoolifyImportError::Api { .. } => {
                ImportError::SourceNotAccessible(error.to_string())
            }
            CoolifyImportError::UnexpectedResponse { .. } => {
                ImportError::DiscoveryFailed(error.to_string())
            }
            CoolifyImportError::WorkloadNotFound { uuid } => {
                ImportError::ContainerNotFound(uuid.clone())
            }
            CoolifyImportError::ProjectResolution { .. } => {
                ImportError::DiscoveryFailed(error.to_string())
            }
        }
    }
}
