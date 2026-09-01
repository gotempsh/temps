// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types for the Portainer importer

use temps_import_types::ImportError;
use thiserror::Error;

/// Errors specific to Portainer API access
#[derive(Error, Debug)]
pub enum PortainerImportError {
    #[error("No Portainer base URL provided: pass the instance URL (e.g. https://1.2.3.4:9443) in credentials.base_url")]
    MissingBaseUrl,

    #[error("No Portainer password provided: pass the admin password in credentials.token")]
    MissingPassword,

    #[error("Portainer base URL '{url}' is not usable: {reason}")]
    InvalidBaseUrl { url: String, reason: String },

    #[error("Portainer API request '{operation}' failed: {reason}")]
    Http { operation: String, reason: String },

    #[error("Portainer rejected the credentials for user '{username}' — check the username and password")]
    LoginFailed { username: String },

    #[error("Portainer API '{operation}' returned HTTP {status}: {body}")]
    Api {
        operation: String,
        status: u16,
        body: String,
    },

    #[error("Portainer API '{operation}' returned a response temps could not parse: {reason}")]
    UnexpectedResponse { operation: String, reason: String },

    #[error("No Portainer environment (endpoint) is available — add a Docker environment in Portainer before importing")]
    NoEndpoint,

    #[error("No Portainer stack or container matching '{id}' was found")]
    WorkloadNotFound { id: String },
}

impl From<PortainerImportError> for ImportError {
    fn from(error: PortainerImportError) -> Self {
        match error {
            PortainerImportError::MissingBaseUrl
            | PortainerImportError::MissingPassword
            | PortainerImportError::InvalidBaseUrl { .. }
            | PortainerImportError::LoginFailed { .. } => {
                ImportError::InvalidCredentials(error.to_string())
            }
            PortainerImportError::Http { .. } | PortainerImportError::Api { .. } => {
                ImportError::SourceNotAccessible(error.to_string())
            }
            PortainerImportError::UnexpectedResponse { .. } | PortainerImportError::NoEndpoint => {
                ImportError::DiscoveryFailed(error.to_string())
            }
            PortainerImportError::WorkloadNotFound { id } => ImportError::ContainerNotFound(id),
        }
    }
}
