//! Error types for the Dokploy importer

use temps_import_types::ImportError;
use thiserror::Error;

/// Errors specific to Dokploy API access
#[derive(Error, Debug)]
pub enum DokployImportError {
    #[error("No Dokploy base URL provided: pass the instance URL (e.g. http://1.2.3.4:3000) in credentials.base_url")]
    MissingBaseUrl,

    #[error("No Dokploy API key provided: create one under Settings → API/CLI (with rate limiting DISABLED — the default is 10 requests/day) and pass it in credentials.token")]
    MissingToken,

    #[error("Dokploy base URL '{url}' is not usable: {reason}")]
    InvalidBaseUrl { url: String, reason: String },

    #[error("Dokploy API request '{operation}' failed: {reason}")]
    Http { operation: String, reason: String },

    #[error("Dokploy rejected the API key during '{operation}' (HTTP 401) — the key may be revoked, expired, or RATE-LIMITED (default keys allow only 10 requests/day; create one with rate limiting disabled)")]
    Unauthorized { operation: String },

    #[error("Dokploy API '{operation}' returned HTTP {status}: {body}")]
    Api {
        operation: String,
        status: u16,
        body: String,
    },

    #[error("Dokploy API '{operation}' returned a response temps could not parse: {reason}")]
    UnexpectedResponse { operation: String, reason: String },

    #[error("No Dokploy application or database with ID '{id}' found on the instance")]
    WorkloadNotFound { id: String },
}

impl From<DokployImportError> for ImportError {
    fn from(error: DokployImportError) -> Self {
        match error {
            DokployImportError::MissingBaseUrl
            | DokployImportError::MissingToken
            | DokployImportError::InvalidBaseUrl { .. }
            | DokployImportError::Unauthorized { .. } => {
                ImportError::InvalidCredentials(error.to_string())
            }
            DokployImportError::Http { .. } | DokployImportError::Api { .. } => {
                ImportError::SourceNotAccessible(error.to_string())
            }
            DokployImportError::UnexpectedResponse { .. } => {
                ImportError::DiscoveryFailed(error.to_string())
            }
            DokployImportError::WorkloadNotFound { id } => ImportError::ContainerNotFound(id),
        }
    }
}
