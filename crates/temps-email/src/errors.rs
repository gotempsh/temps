// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

    #[error("Domain '{domain}' already exists for provider {provider_id}")]
    DomainAlreadyExists { domain: String, provider_id: i32 },

    #[error("Email domain '{domain}' is not authorized for project {project_id}")]
    DomainNotAuthorized { domain: String, project_id: i32 },

    #[error("Project not found: {0}")]
    ProjectNotFound(i32),

    #[error("Idempotency key '{key}' was reused with a different email payload")]
    IdempotencyConflict { key: String },

    #[error("Invalid provider type: {0}")]
    InvalidProviderType(String),

    #[error("Provider error: {0}")]
    ProviderError(String),

    /// The provider request may have been accepted, but no definitive response
    /// reached Temps. Retrying this outcome could deliver a duplicate email.
    #[error("Provider delivery outcome is unknown: {0}")]
    ProviderDeliveryUnknown(String),

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

    #[error("Failed to build Scaleway HTTP client")]
    ScalewayClientBuild {
        #[source]
        source: reqwest::Error,
    },

    #[error("SMTP error: {0}")]
    Smtp(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Tracking rewrite failed for email {email_id}: {reason}")]
    TrackingRewrite { email_id: String, reason: String },

    /// A definitive provider rejection where the provider confirmed it did not
    /// accept the message. `retryable` distinguishes transient throttles
    /// (e.g. TooManyRequestsException, 4xx SMTP reply codes) from permanent
    /// rejections (MessageRejected, 5xx SMTP). Only constructed inside each
    /// provider's `send()` method; never used for domain-verification paths.
    #[error("Provider '{provider}' send rejected: {message}")]
    SendFailed {
        provider: String,
        retryable: bool,
        message: String,
    },

    /// The provider's API definitively rejected the supplied credentials during
    /// the read-only verification check that runs before persisting a new or
    /// updated provider. This is a client input error, not an internal failure —
    /// the user typed the wrong key, secret, or project ID.
    #[error("Invalid {provider_type} credentials: {reason}")]
    InvalidCredentials {
        provider_type: String,
        reason: String,
    },

    /// A lightweight credential check could not reach the provider's API,
    /// most likely due to a network or DNS issue on the Temps host. The
    /// credentials themselves may be valid. The operator should verify that
    /// the Temps server can reach the provider's API endpoint.
    #[error("Could not reach {provider_type} to verify credentials: {reason}")]
    ProviderUnreachable {
        provider_type: String,
        reason: String,
    },
}

impl From<serde_json::Error> for EmailError {
    fn from(err: serde_json::Error) -> Self {
        EmailError::Serialization(err.to_string())
    }
}
