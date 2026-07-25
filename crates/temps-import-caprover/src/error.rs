//! Error types for the CapRover importer

use temps_import_types::ImportError;
use thiserror::Error;

/// Errors specific to CapRover API access
#[derive(Error, Debug)]
pub enum CaproverImportError {
    #[error("No CapRover base URL provided: pass the instance URL (e.g. http://1.2.3.4:3000) in credentials.base_url")]
    MissingBaseUrl,

    #[error("No CapRover password provided: pass the admin password in credentials.token")]
    MissingPassword,

    #[error("CapRover base URL '{url}' is not usable: {reason}")]
    InvalidBaseUrl { url: String, reason: String },

    #[error("CapRover API request '{operation}' failed: {reason}")]
    Http { operation: String, reason: String },

    #[error("CapRover rejected the password during login — check the admin password (the install default is 'captain42' until changed)")]
    LoginFailed,

    #[error("CapRover API '{operation}' returned status {status}: {description}")]
    Api {
        operation: String,
        status: i64,
        description: String,
    },

    #[error("CapRover API '{operation}' returned a response temps could not parse: {reason}")]
    UnexpectedResponse { operation: String, reason: String },

    #[error("No CapRover app named '{app_name}' found on the instance")]
    AppNotFound { app_name: String },
}

impl From<CaproverImportError> for ImportError {
    fn from(error: CaproverImportError) -> Self {
        match error {
            CaproverImportError::MissingBaseUrl
            | CaproverImportError::MissingPassword
            | CaproverImportError::InvalidBaseUrl { .. }
            | CaproverImportError::LoginFailed => {
                ImportError::InvalidCredentials(error.to_string())
            }
            CaproverImportError::Http { .. } | CaproverImportError::Api { .. } => {
                ImportError::SourceNotAccessible(error.to_string())
            }
            CaproverImportError::UnexpectedResponse { .. } => {
                ImportError::DiscoveryFailed(error.to_string())
            }
            CaproverImportError::AppNotFound { app_name } => {
                ImportError::ContainerNotFound(app_name)
            }
        }
    }
}
