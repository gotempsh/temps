//! DNS provider error types

use thiserror::Error;

/// DNS provider errors
#[derive(Error, Debug)]
pub enum DnsError {
    #[error("Provider not found: {0}")]
    ProviderNotFound(i32),

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

    #[error("DNS record conflict for {record_type} '{name}' in zone {domain}: {reason}. Temps never overwrites a record it does not manage — import the record into temps management from the domain's DNS settings, or remove it at the provider and retry")]
    RecordConflict {
        domain: String,
        name: String,
        record_type: String,
        reason: String,
    },

    #[error("DNS record {record_type} '{name}' in zone {domain} is owned by temps instance '{owner_instance}', not this one; refusing to modify it")]
    NotOwnedByInstance {
        domain: String,
        name: String,
        record_type: String,
        owner_instance: String,
    },

    #[error("Cannot create proxied record '{fqdn}': it sits {levels} subdomain levels below the zone apex, and Cloudflare Universal SSL only covers one level, so TLS would fail at the edge (error 526) without Advanced Certificate Manager. Use the flat public hostname strategy instead (e.g. '{flat_suggestion}'), or disable proxying for this record")]
    ProxiedDepthUnsupported {
        fqdn: String,
        levels: usize,
        flat_suggestion: String,
    },

    #[error("DNS provider '{provider}' does not support proxied records; disable proxying for this record or use a provider with proxy support (e.g. Cloudflare)")]
    ProxyNotSupportedByProvider { provider: String },
}
