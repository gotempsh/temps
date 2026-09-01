// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types for the Kamal importer

use temps_import_types::ImportError;
use thiserror::Error;

/// Errors specific to reading a Kamal deploy configuration
#[derive(Error, Debug)]
pub enum KamalImportError {
    #[error("No Kamal configuration provided: paste the contents of config/deploy.yml into credentials.extra[\"deploy_yml\"]")]
    MissingConfig,

    #[error("config/deploy.yml is not valid YAML: {reason}")]
    InvalidYaml { reason: String },

    #[error("config/deploy.yml is missing the required '{field}' field")]
    MissingField { field: &'static str },

    #[error(
        "config/deploy.yml uses ERB (<%= ... %>) in '{field}', which temps cannot evaluate — run 'kamal config' and paste the resolved output instead"
    )]
    ErbTemplating { field: String },

    #[error("No Kamal workload named '{name}' in this configuration")]
    WorkloadNotFound { name: String },
}

impl From<KamalImportError> for ImportError {
    fn from(error: KamalImportError) -> Self {
        match error {
            KamalImportError::MissingConfig => ImportError::InvalidCredentials(error.to_string()),
            KamalImportError::InvalidYaml { .. }
            | KamalImportError::MissingField { .. }
            | KamalImportError::ErbTemplating { .. } => {
                ImportError::InvalidConfiguration(error.to_string())
            }
            KamalImportError::WorkloadNotFound { name } => ImportError::ContainerNotFound(name),
        }
    }
}
