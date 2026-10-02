// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DNS provider error types

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

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

    #[error("Delivery profile {profile_id} not found")]
    DeliveryProfileNotFound { profile_id: i32 },

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

    #[error("DNS record conflict for {record_type} '{name}' in zone {domain}: {reason}. Temps never overwrites a record it does not manage — import the record into temps management from the domain's DNS settings, or remove it at the provider and retry")]
    RecordConflict {
        domain: String,
        name: String,
        record_type: String,
        reason: String,
    },

    #[error("{resource} {id} ('{name}') is in use: {reason}")]
    ResourceInUse {
        resource: &'static str,
        id: i32,
        name: String,
        reason: String,
    },

    #[error("DNS record {record_type} '{name}' in zone {domain} is owned by temps instance '{owner_instance}', not this one; refusing to modify it")]
    NotOwnedByInstance {
        domain: String,
        name: String,
        record_type: String,
        owner_instance: String,
    },

    /// The record (or its orphan marker) is owned by this install but under a
    /// different controller/project/environment than the caller's. Boxed to
    /// keep `DnsError` small.
    #[error("{0}")]
    OwnedByOtherScope(Box<OwnershipScopeConflict>),

    #[error("Another DNS operation is already running for '{name}' in zone {zone}; retry when it completes")]
    RecordLocked { zone: String, name: String },

    /// Another operation that rewrites the zone's generated-hostname state
    /// (its DNS records, record states and hostname mode) holds the zone
    /// operation lock. Retryable, like [`DnsError::RecordLocked`].
    #[error("Another generated-hostname operation is already running for zone '{zone}' on DNS provider {provider_id}; retry when it completes")]
    ZoneOperationInProgress { provider_id: i32, zone: String },

    #[error("Cannot create proxied record '{fqdn}': it sits {levels} subdomain levels below the zone apex, and Cloudflare Universal SSL only covers one level, so TLS would fail at the edge (error 526) without Advanced Certificate Manager. Use the flat public hostname strategy instead (e.g. '{flat_suggestion}'), or disable proxying for this record")]
    ProxiedDepthUnsupported {
        fqdn: String,
        levels: usize,
        flat_suggestion: String,
    },

    #[error("DNS provider '{provider}' does not support proxied records; disable proxying for this record or use a provider with proxy support (e.g. Cloudflare)")]
    ProxyNotSupportedByProvider { provider: String },

    /// A domain delivery apply found its DNS provider or managed zone gone,
    /// deactivated, unverified, or no longer auto-managed when it went to
    /// reserve the binding.
    #[error("Domain delivery for '{hostname}' cannot use zone '{zone}' on DNS provider {provider_id}: {reason}; create a new preview once the zone is managed again")]
    DeliveryZoneUnavailable {
        hostname: String,
        zone: String,
        provider_id: i32,
        reason: String,
    },

    /// A domain delivery apply or binding cleanup failed after it had already
    /// changed routing, DNS, or CDN state. Carries what completed so the
    /// attempt can be audited and resumed. Boxed to keep `DnsError` small.
    #[error("{0}")]
    DeliveryIncomplete(Box<DeliveryIncomplete>),
}

/// A state-changing step of a domain delivery apply or binding cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStep {
    /// A project custom domain (route) was created for the hostname.
    CustomDomainCreated,
    /// The delivery binding row was saved before any provider call.
    BindingReserved,
    /// The hostname was added to the Bunny Pull Zone.
    BunnyHostnameAttached,
    /// A pre-existing provider record was adopted into Temps management.
    DnsRecordAdopted,
    /// The DNS record was written at the provider.
    DnsRecordWritten,
    /// The provider read back exactly the record that was written.
    DnsRecordVerified,
    /// Bunny was asked to issue the hostname's edge certificate.
    CertificateRequested,
    /// The binding and its preview were marked as applied.
    BindingActivated,
    /// The binding's DNS record was removed at the provider.
    DnsRecordRemoved,
    /// The hostname was detached from the Bunny Pull Zone.
    BunnyHostnameRemoved,
    /// The binding row was deleted.
    BindingDeleted,
}

impl DeliveryStep {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CustomDomainCreated => "custom_domain_created",
            Self::BindingReserved => "binding_reserved",
            Self::BunnyHostnameAttached => "bunny_hostname_attached",
            Self::DnsRecordAdopted => "dns_record_adopted",
            Self::DnsRecordWritten => "dns_record_written",
            Self::DnsRecordVerified => "dns_record_verified",
            Self::CertificateRequested => "certificate_requested",
            Self::BindingActivated => "binding_activated",
            Self::DnsRecordRemoved => "dns_record_removed",
            Self::BunnyHostnameRemoved => "bunny_hostname_removed",
            Self::BindingDeleted => "binding_deleted",
        }
    }
}

impl fmt::Display for DeliveryStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The delivery operation a [`DeliveryIncomplete`] interrupted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOperation {
    /// `POST /projects/{project_id}/domain-delivery-bindings/apply`
    Apply,
    /// `DELETE /projects/{project_id}/domain-delivery-bindings/{binding_id}`
    Cleanup,
}

impl DeliveryOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Cleanup => "cleanup",
        }
    }
}

impl fmt::Display for DeliveryOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Details of a [`DnsError::DeliveryIncomplete`]: a delivery apply or cleanup
/// that failed at `failed_step` after `completed_steps` had already changed
/// state. `source` is the error that stopped it and decides the HTTP status.
#[derive(Error, Debug)]
#[error(
    "Domain delivery {operation} for '{hostname}' (project {project_id}, environment {environment_id}{}) failed at step '{failed_step}' after completing {}; {}: {source}",
    describe_delivery_references(.preview_id, .binding_id),
    describe_delivery_steps(.completed_steps),
    describe_delivery_recovery(*.operation)
)]
pub struct DeliveryIncomplete {
    pub operation: DeliveryOperation,
    pub project_id: i32,
    pub environment_id: i32,
    pub hostname: String,
    pub preview_id: Option<Uuid>,
    pub binding_id: Option<i32>,
    pub completed_steps: Vec<DeliveryStep>,
    pub failed_step: DeliveryStep,
    pub source: DnsError,
}

fn describe_delivery_references(preview_id: &Option<Uuid>, binding_id: &Option<i32>) -> String {
    let mut references = String::new();
    if let Some(preview_id) = preview_id {
        references.push_str(&format!(", preview {preview_id}"));
    }
    if let Some(binding_id) = binding_id {
        references.push_str(&format!(", binding {binding_id}"));
    }
    references
}

fn describe_delivery_steps(steps: &[DeliveryStep]) -> String {
    let names: Vec<&str> = steps.iter().map(|step| step.as_str()).collect();
    format!("[{}]", names.join(", "))
}

fn describe_delivery_recovery(operation: DeliveryOperation) -> &'static str {
    match operation {
        DeliveryOperation::Apply => {
            "completed changes were kept; apply the same preview again to resume"
        }
        DeliveryOperation::Cleanup => {
            "the binding was kept as cleanup_failed; delete it again to finish cleanup"
        }
    }
}

/// Details of a [`DnsError::OwnedByOtherScope`] refusal.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[error(
    "DNS record {record_type} '{name}' in zone {zone} is managed by temps for {}, not for the requesting {}; refusing to modify it. Change or remove it through the workflow that owns it",
    describe_ownership_scope(.owner_controller, .owner_project_id, .owner_environment_id),
    describe_ownership_scope(.requester_controller, .requester_project_id, .requester_environment_id)
)]
pub struct OwnershipScopeConflict {
    pub zone: String,
    pub name: String,
    pub record_type: String,
    pub owner_controller: Option<String>,
    pub owner_project_id: Option<i32>,
    pub owner_environment_id: Option<i32>,
    pub requester_controller: Option<String>,
    pub requester_project_id: Option<i32>,
    pub requester_environment_id: Option<i32>,
}

/// Render an ownership scope (controller, project, environment) for error
/// messages, e.g. `controller 'domain-delivery' (project 7, environment 42)`
/// or `the generic DNS records API (no project, no environment)`.
fn describe_ownership_scope(
    controller: &Option<String>,
    project_id: &Option<i32>,
    environment_id: &Option<i32>,
) -> String {
    let controller = match controller {
        Some(controller) => format!("controller '{controller}'"),
        None => "the generic DNS records API".to_string(),
    };
    let project = match project_id {
        Some(id) => format!("project {id}"),
        None => "no project".to_string(),
    };
    let environment = match environment_id {
        Some(id) => format!("environment {id}"),
        None => "no environment".to_string(),
    };
    format!("{controller} ({project}, {environment})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_by_other_scope_message_names_both_scopes() {
        let error = DnsError::OwnedByOtherScope(Box::new(OwnershipScopeConflict {
            zone: "example.com".into(),
            name: "app".into(),
            record_type: "A".into(),
            owner_controller: Some("domain-delivery".into()),
            owner_project_id: Some(7),
            owner_environment_id: Some(42),
            requester_controller: None,
            requester_project_id: None,
            requester_environment_id: None,
        }));
        let message = error.to_string();
        assert!(message.contains("A 'app' in zone example.com"));
        assert!(message.contains("controller 'domain-delivery' (project 7, environment 42)"));
        assert!(message.contains("the generic DNS records API (no project, no environment)"));
    }

    #[test]
    fn record_locked_message_says_to_retry() {
        let error = DnsError::RecordLocked {
            zone: "example.com".into(),
            name: "app".into(),
        };
        let message = error.to_string();
        assert!(message.contains("'app' in zone example.com"));
        assert!(message.contains("retry"));
    }

    #[test]
    fn zone_operation_in_progress_message_names_provider_and_zone() {
        let error = DnsError::ZoneOperationInProgress {
            provider_id: 7,
            zone: "example.com".into(),
        };
        let message = error.to_string();
        assert!(message.contains("zone 'example.com'"), "{message}");
        assert!(message.contains("DNS provider 7"), "{message}");
        assert!(message.contains("retry"), "{message}");
    }
}
