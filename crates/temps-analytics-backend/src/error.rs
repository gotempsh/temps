use thiserror::Error;

/// Error type for any analytics backend implementation.
///
/// Each variant must include enough context (project_id, query name, underlying
/// reason) to debug from logs alone. New variants should follow the same rule:
/// no bare strings, no generic catch-all variants.
#[derive(Debug, Error)]
pub enum AnalyticsBackendError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Analytics query '{query}' not found for project {project_id}")]
    NotFound { project_id: i32, query: String },

    #[error("Invalid input for analytics query '{query}': {reason}")]
    InvalidInput { query: String, reason: String },

    #[error("Validation error in analytics query '{query}': {reason}")]
    Validation { query: String, reason: String },

    #[error("Backend '{backend}' is unavailable: {reason}")]
    BackendUnavailable { backend: String, reason: String },
}
