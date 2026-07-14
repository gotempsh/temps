//! Error types for the email service

use thiserror::Error;

#[derive(Error, Debug)]
pub enum EmailError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Provider not found: {0}")]
    ProviderNotFound(i32),

    #[error("Domain not found: {0}")]
    DomainNotFound(i32),

    #[error("Email not found: {0}")]
    EmailNotFound(String),

    #[error("Domain not verified: {0}")]
    DomainNotVerified(String),

    #[error("Invalid provider type: {0}")]
    InvalidProviderType(String),

    #[error("Provider error: {0}")]
    ProviderError(String),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Decryption error: {0}")]
    Decryption(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("AWS SES error: {0}")]
    AwsSes(String),

    #[error("Scaleway error: {0}")]
    Scaleway(String),

    #[error("SMTP error: {0}")]
    Smtp(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Tracking rewrite failed for email {email_id}: {reason}")]
    TrackingRewrite { email_id: String, reason: String },

    #[error(
        "Failed to redact email tracking data before {cutoff} in batches of {batch_size}: {source}"
    )]
    TrackingRetentionRedaction {
        cutoff: chrono::DateTime<chrono::Utc>,
        batch_size: i64,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Tracking event retention must be at least 1 day; received {days}")]
    InvalidTrackingRetentionDays { days: u32 },

    #[error(
        "Email tracking retention index is not ready; deferring connection-metadata redaction"
    )]
    TrackingRetentionIndexUnavailable,

    #[error(
        "Failed to manage the email tracking retention scheduler: task state lock is poisoned"
    )]
    TrackingRetentionSchedulerState,
}

impl From<serde_json::Error> for EmailError {
    fn from(err: serde_json::Error) -> Self {
        EmailError::Serialization(err.to_string())
    }
}
