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

    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<serde_json::Error> for EmailError {
    fn from(err: serde_json::Error) -> Self {
        EmailError::Serialization(err.to_string())
    }
}
