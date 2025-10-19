//! Sentry-specific request/response types

use sea_orm::DbErr;
use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SentryEventRequest {
    pub event_id: Option<String>,
    pub timestamp: Option<String>,
    pub platform: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct SentryEventResponse {
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateDSNRequest {
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DSNResponse {
    pub id: i32,
    pub dsn: String,
    pub public_key: String,
    pub project_id: i32,
    pub created_at: String,
    pub is_active: bool,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
}

// ===== Error Types =====

#[derive(Error, Debug)]
pub enum SentryIngesterError {
    #[error("Database error: {0}")]
    Database(#[from] DbErr),

    #[error("Project not found")]
    ProjectNotFound,

    #[error("Invalid DSN")]
    InvalidDSN,

    #[error("Validation error: {0}")]
    Validation(String),
}

// ===== DSN Domain Types =====

/// DSN (Data Source Name) for error tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDSN {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub name: String,
    pub public_key: String,
    pub secret_key: String,
    pub dsn: String, // Public DSN without secret: protocol://public_key@host:port/project_id
    pub created_at: UtcDateTime,
    pub is_active: bool,
    pub event_count: i64,
}

/// Parsed DSN components
#[derive(Debug, Clone)]
pub struct ParsedDSN {
    pub public_key: String,
    pub project_id: i32,
    pub host: String,
    pub protocol: String,
}
