//! Typed errors for the feature-flag domain.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum FlagError {
    #[error("Feature flag '{key}' not found in project {project_id}")]
    NotFound { project_id: i32, key: String },

    #[error("Feature flag '{key}' already exists in project {project_id}")]
    DuplicateKey { project_id: i32, key: String },

    #[error("Environment {environment_id} does not belong to project {project_id}")]
    EnvironmentNotInProject {
        project_id: i32,
        environment_id: i32,
    },

    #[error("Environment {environment_id} not found")]
    EnvironmentNotFound { environment_id: i32 },

    #[error(
        "Invalid flag key '{key}': {reason}. Keys must match [a-z0-9][a-z0-9._-]* and be at most 128 characters"
    )]
    InvalidKey { key: String, reason: String },

    #[error("Invalid value type '{value_type}' for flag '{key}': must be one of bool, string, number, json")]
    InvalidValueType { key: String, value_type: String },

    #[error("Value {value} is not valid for flag '{key}' of type '{value_type}' (field: {field})")]
    ValueTypeMismatch {
        key: String,
        value_type: String,
        value: String,
        field: String,
    },

    #[error("Validation error for flag '{key}': {message}")]
    Validation { key: String, message: String },

    #[error("Deployment token for project {project_id} is not scoped to a single environment; feature-flag snapshots require an environment-scoped token")]
    TokenNotEnvironmentScoped { project_id: i32 },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Failed to serialize flag value for flag '{key}': {reason}")]
    Serialization { key: String, reason: String },
}
