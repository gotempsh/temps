// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DNS provider error types

use thiserror::Error;

/// DNS provider errors
#[derive(Error, Debug)]
pub enum DnsError {
    #[error("Provider not found: {0}")]
    ProviderNotFound(i32),

    #[error("DNS provider {provider_id} ({provider_name}) is inactive")]
    ProviderInactive {
        provider_id: i32,
        provider_name: String,
    },

    #[error("Invalid provider type: {0}")]
    InvalidProviderType(String),

    #[error("Invalid credentials: {0}")]
    InvalidCredentials(String),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Decryption error: {0}")]
    Decryption(String),

    #[error("Zone not found: {0}")]
    ZoneNotFound(String),

    #[error("Domain not found: {0}")]
    DomainNotFound(String),

    #[error(
        "Managed DNS domain '{requested_domain}' canonicalizes to '{canonical_domain}', which is already managed by domain ID {existing_managed_domain_id} on provider {existing_provider_id}"
    )]
    ManagedDomainAlreadyExists {
        requested_domain: String,
        canonical_domain: String,
        existing_managed_domain_id: i32,
        existing_provider_id: i32,
    },

    #[error(
        "Ambiguous managed DNS zone '{canonical_zone}' for requested domain '{requested_domain}': managed domain IDs {managed_domain_ids:?} use provider IDs {provider_ids:?}"
    )]
    AmbiguousManagedDomain {
        requested_domain: String,
        canonical_zone: String,
        managed_domain_ids: Vec<i32>,
        provider_ids: Vec<i32>,
    },

    #[error("Record not found: {0}")]
    RecordNotFound(String),

    #[error("API error: {0}")]
    ApiError(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Request error: {0}")]
    Request(#[from] reqwest::Error),

    #[error("Provider does not manage domain: {0}")]
    DomainNotManaged(String),

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
}
