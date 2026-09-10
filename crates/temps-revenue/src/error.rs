// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

use crate::providers::ProviderError;

/// Top-level error type for the revenue crate.
///
/// Every variant carries contextual identifiers so log entries are
/// immediately actionable (per the project's error-handling standard).
#[derive(Debug, Error)]
pub enum RevenueError {
    #[error("Revenue integration {integration_id} not found in project {project_id}")]
    IntegrationNotFound {
        integration_id: i32,
        project_id: i32,
    },

    #[error("Revenue integration not found for webhook path token")]
    IntegrationNotFoundByToken,

    #[error(
        "Webhook URL provider '{url_provider}' does not match integration provider '{integration_provider}' (integration {integration_id})"
    )]
    ProviderMismatch {
        integration_id: i32,
        url_provider: String,
        integration_provider: String,
    },

    #[error("Unknown revenue provider '{provider}'")]
    UnknownProvider { provider: String },

    #[error(
        "Project {project_id} already has an active integration for provider '{provider}' (id {existing_integration_id}); disconnect it before creating a new one"
    )]
    DuplicateIntegration {
        project_id: i32,
        provider: String,
        existing_integration_id: i32,
    },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Integration {integration_id} is disabled")]
    IntegrationDisabled { integration_id: i32 },

    #[error("Provider error while processing webhook for integration {integration_id}: {source}")]
    Provider {
        integration_id: i32,
        #[source]
        source: ProviderError,
    },

    #[error("Failed to encrypt signing secret for project {project_id}: {reason}")]
    EncryptionFailed { project_id: i32, reason: String },

    #[error("Failed to decrypt signing secret for integration {integration_id}: {reason}")]
    DecryptionFailed { integration_id: i32, reason: String },

    #[error("OS randomness failed while {operation}: {reason}")]
    RandomnessFailed { operation: String, reason: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}
