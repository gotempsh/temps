// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types for the import system

use thiserror::Error;

/// Result type for import operations
pub type ImportResult<T> = Result<T, ImportError>;

/// Errors that can occur during import operations
#[derive(Error, Debug)]
pub enum ImportError {
    /// Source system is not accessible (e.g., Docker daemon not running)
    #[error("Source not accessible: {0}")]
    SourceNotAccessible(String),

    /// Container not found in source system
    #[error("Container not found: {0}")]
    ContainerNotFound(String),

    /// Failed to discover containers
    #[error("Discovery failed: {0}")]
    DiscoveryFailed(String),

    /// Failed to inspect/describe container
    #[error("Inspection failed: {0}")]
    InspectionFailed(String),

    /// Invalid configuration detected
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Validation failed (pre-flight checks)
    #[error("Validation failed: {0}")]
    ValidationFailed(String),

    /// Plan generation failed
    #[error("Plan generation failed: {0}")]
    PlanGenerationFailed(String),

    /// Execution failed
    #[error("Execution failed: {0}")]
    ExecutionFailed(String),

    /// A specific migration step failed
    #[error("Step '{step_name}' failed: {message}")]
    StepFailed {
        step_name: String,
        step_index: usize,
        message: String,
        /// Whether partial results were committed before this failure
        partial_commit: bool,
    },

    /// Unsupported feature for this source
    #[error("Unsupported feature: {0}")]
    UnsupportedFeature(String),

    /// Network configuration error
    #[error("Network error: {0}")]
    NetworkError(String),

    /// Volume/storage error
    #[error("Volume error: {0}")]
    VolumeError(String),

    /// Authentication/authorization error
    #[error("Authentication error: {0}")]
    AuthenticationError(String),

    /// Credential validation error
    #[error("Invalid credentials: {0}")]
    InvalidCredentials(String),

    /// Rate limit exceeded on source platform
    #[error("Rate limit exceeded: {0}")]
    RateLimitExceeded(String),

    /// Service migration error (database, cache, etc.)
    #[error("Service migration error: {0}")]
    ServiceMigrationError(String),

    /// Domain migration error
    #[error("Domain migration error: {0}")]
    DomainMigrationError(String),

    /// Generic internal error
    #[error("Internal error: {0}")]
    Internal(String),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}
