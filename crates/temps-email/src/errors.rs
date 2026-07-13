//! Error types for the email service

use thiserror::Error;

use crate::providers::EmailProviderType;

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

    /// A provider's `send()` call failed. Unlike the flat per-provider
    /// variants above (used for identity/domain management calls),
    /// this carries a `retryable` classification derived from the
    /// underlying transport error (HTTP status, SMTP reply code, or AWS SDK
    /// error kind) so the send path knows whether retrying — same provider
    /// or the next one in the failover chain — can plausibly succeed.
    #[error("Failed to send email via {provider}: {reason}")]
    SendFailed {
        provider: EmailProviderType,
        retryable: bool,
        reason: String,
    },
}

impl From<serde_json::Error> for EmailError {
    fn from(err: serde_json::Error) -> Self {
        EmailError::Serialization(err.to_string())
    }
}

impl EmailError {
    /// Whether this failure is transient and worth retrying (against the
    /// same provider or the next one in a domain's failover chain). Every
    /// variant besides `SendFailed { retryable: true, .. }` represents a
    /// config, validation, or lookup problem that a retry cannot fix.
    pub fn is_retryable(&self) -> bool {
        matches!(self, EmailError::SendFailed { retryable: true, .. })
    }
}
