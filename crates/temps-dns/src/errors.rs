// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DNS provider error types

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::services::hostname_sync::RecordChange;

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

    /// `reason` says what conflicts and, where there is one, how to resolve
    /// it: the remedy differs by caller (adopting a record, a new preview,
    /// removing a record at the provider), so none is appended here.
    #[error("DNS record conflict for {record_type} '{name}' in zone {domain}: {reason}")]
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

    /// A generated-hostname sync found desired hostnames whose records it may
    /// not write — records Temps does not manage, or manages for another
    /// owner — and no adopt or skip decision for them. Nothing was changed. A
    /// preview lists each conflict and whether it can be adopted.
    #[error(
        "Cannot sync generated hostnames in zone '{zone}': {}. Temps never overwrites a record it does not manage, so nothing was changed; preview the hostname mode again and adopt or skip each conflicting record",
        describe_hostname_conflicts(.conflicts)
    )]
    GeneratedHostnameConflicts {
        zone: String,
        /// `"<type> '<hostname>': <reason>"` per conflict.
        conflicts: Vec<String>,
    },

    /// An adopt or skip decision sent with a hostname-mode apply does not
    /// match the zone any more: its record stopped conflicting, changed after
    /// the preview, or cannot be adopted. Nothing was changed.
    #[error("The {decision} decision for {record_type} record '{name}' in zone '{zone}' no longer applies: {reason}. Nothing was changed; preview the hostname mode again to review the record's current state")]
    HostnameDecisionRejected {
        zone: String,
        name: String,
        record_type: String,
        /// `"adopt"` or `"skip"`.
        decision: &'static str,
        reason: String,
    },

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

    /// A domain delivery apply found its project row gone when it went to
    /// reserve the binding: the project was deleted after apply checked it.
    #[error(
        "Project {project_id} not found; domain delivery for '{hostname}' cannot be reserved in it"
    )]
    DeliveryProjectNotFound { project_id: i32, hostname: String },

    /// A domain delivery apply found its project marked for deletion when it
    /// went to reserve the binding. Project deletion locks the same row before
    /// it counts bindings, so no binding is reserved behind its fence.
    #[error("Project {project_id} is being deleted; domain delivery for '{hostname}' cannot be reserved in it")]
    DeliveryProjectBeingDeleted { project_id: i32, hostname: String },

    /// A domain delivery apply found its environment row gone, or no longer
    /// in the project, when it went to reserve the binding.
    #[error("Environment {environment_id} not found in project {project_id}; domain delivery for '{hostname}' cannot be reserved in it")]
    DeliveryEnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
        hostname: String,
    },

    /// A domain delivery apply found its environment deleted when it went to
    /// reserve the binding. Environment deletion locks the same row before it
    /// counts bindings, so no binding is reserved behind its soft delete.
    #[error("Environment {environment_id} of project {project_id} was deleted; domain delivery for '{hostname}' cannot be reserved in it")]
    DeliveryEnvironmentDeleted {
        project_id: i32,
        environment_id: i32,
        hostname: String,
    },

    /// A domain delivery apply or binding cleanup failed after it had already
    /// changed routing, DNS, or CDN state. Carries what completed so the
    /// attempt can be audited and resumed. Boxed to keep `DnsError` small.
    #[error("{0}")]
    DeliveryIncomplete(Box<DeliveryIncomplete>),

    /// A guarded record write changed the record at the provider, then could
    /// not finalize the record's ownership marker. Boxed to keep `DnsError`
    /// small.
    #[error("{0}")]
    ManagedRecordMarkerNotFinalized(Box<MarkerNotFinalized>),

    /// A hostname-mode apply stopped after it had already changed DNS
    /// records. Carries what completed so the attempt can be audited and
    /// finished. Boxed to keep `DnsError` small.
    #[error("{0}")]
    HostnameModeIncomplete(Box<HostnameModeIncomplete>),
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

/// How many conflicts a [`DnsError::GeneratedHostnameConflicts`] message
/// names; the rest are only counted, so a zone with hundreds of conflicts
/// still produces a readable error.
const NAMED_HOSTNAME_CONFLICTS: usize = 3;

fn describe_hostname_conflicts(conflicts: &[String]) -> String {
    let count = conflicts.len();
    let named = conflicts
        .iter()
        .take(NAMED_HOSTNAME_CONFLICTS)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("; ");
    let counted = if count == 1 {
        "1 record conflicts".to_string()
    } else {
        format!("{count} records conflict")
    };
    match count.saturating_sub(NAMED_HOSTNAME_CONFLICTS) {
        0 => format!("{counted} ({named})"),
        more => format!("{counted} ({named}; and {more} more)"),
    }
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

/// Details of a [`DnsError::ManagedRecordMarkerNotFinalized`]: the record
/// write succeeded and `source` is the error of the marker write after it,
/// which decides the HTTP status.
#[derive(Error, Debug)]
#[error(
    "{record_type} record '{name}' in zone {zone} was written, but its ownership marker could not be finalized afterwards; {}: {source}",
    describe_unfinalized_marker(*.stays_managed)
)]
pub struct MarkerNotFinalized {
    pub zone: String,
    pub name: String,
    pub record_type: String,
    /// Whether the marker written before the record changed already covers
    /// the value the provider stored, so the record stays managed.
    pub stays_managed: bool,
    /// Whether the provider stored the record proxied.
    pub proxied: bool,
    pub source: DnsError,
}

fn describe_unfinalized_marker(stays_managed: bool) -> &'static str {
    if stays_managed {
        "the marker written before the change already covers the new value, so the record stays managed by this install and its next write finalizes the marker"
    } else {
        "the provider stored a value other than the one requested, which the earlier marker does not cover, so the record is no longer recognized as managed; check it at the provider"
    }
}

/// Details of a [`DnsError::HostnameModeIncomplete`]: an apply of `mode`
/// that failed after `completed` DNS changes, which stay in place. `source`
/// is the error that stopped it and decides the HTTP status.
#[derive(Error, Debug)]
#[error(
    "Applying hostname mode '{mode}' to zone '{zone}' on DNS provider {provider_id} stopped after {}; {}: {source}",
    describe_dns_changes(.completed),
    describe_hostname_mode_recovery(*.saved)
)]
pub struct HostnameModeIncomplete {
    pub provider_id: i32,
    pub zone: String,
    pub mode: String,
    /// DNS changes that completed before the failure, in order.
    pub completed: Vec<RecordChange>,
    /// What the apply saved before it returned.
    pub saved: HostnameModeSaved,
    pub source: DnsError,
}

/// What a stopped hostname-mode apply saved before it returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostnameModeSaved {
    /// The new mode, with the record states of the records it uses. The
    /// apply stopped while removing records the new mode no longer uses.
    Mode,
    /// The record states of the changes made. The mode is unchanged.
    RecordStates,
    /// Nothing: the mode is unchanged, and saving the record states of the
    /// changes made failed too.
    Nothing,
}

/// At most this many completed changes are named in an error message; the
/// audit entry and the reconciliation run keep the complete list.
const DESCRIBED_DNS_CHANGES: usize = 10;

fn describe_dns_changes(changes: &[RecordChange]) -> String {
    if changes.is_empty() {
        return "changing no DNS record".to_string();
    }
    let named: Vec<String> = changes
        .iter()
        .take(DESCRIBED_DNS_CHANGES)
        .map(|change| format!("{} {} {}", change.action, change.record_type, change.name))
        .collect();
    let more = changes.len().saturating_sub(DESCRIBED_DNS_CHANGES);
    let suffix = if more > 0 {
        format!(", and {more} more")
    } else {
        String::new()
    };
    format!(
        "{} DNS change(s): {}{suffix}",
        changes.len(),
        named.join(", ")
    )
}

fn describe_hostname_mode_recovery(saved: HostnameModeSaved) -> &'static str {
    match saved {
        HostnameModeSaved::Mode => {
            "the hostname mode and its record states were saved, but records the new mode no longer uses may remain; apply the mode again to remove them"
        }
        HostnameModeSaved::RecordStates => {
            "the hostname mode was not changed and the changes made were kept, with their record states saved for origin certificates; apply the mode again to finish, or apply the previous mode to undo them"
        }
        HostnameModeSaved::Nothing => {
            "the hostname mode was not changed and the changes made were kept, but saving their record states failed too, so origin certificates may not cover them yet; apply the mode again to finish, or apply the previous mode to undo them"
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

    #[test]
    fn record_conflict_message_carries_only_its_own_remedy() {
        let error = DnsError::RecordConflict {
            domain: "example.com".into(),
            name: "app".into(),
            record_type: "A".into(),
            reason: "routing changed after preview; create a new preview".into(),
        };
        assert_eq!(
            error.to_string(),
            "DNS record conflict for A 'app' in zone example.com: routing changed after preview; create a new preview"
        );
    }

    #[test]
    fn generated_hostname_conflicts_message_names_the_first_conflicts_and_counts_the_rest() {
        let conflict = |index: usize| format!("A 'pr-{index}.example.com': no marker");
        let message = |count: usize| {
            DnsError::GeneratedHostnameConflicts {
                zone: "example.com".into(),
                conflicts: (1..=count).map(conflict).collect(),
            }
            .to_string()
        };

        let one = message(1);
        assert!(
            one.contains(
                "zone 'example.com': 1 record conflicts (A 'pr-1.example.com': no marker)"
            ),
            "{one}"
        );
        assert!(
            one.contains("adopt or skip each conflicting record"),
            "{one}"
        );

        let three = message(3);
        assert!(three.contains("3 records conflict ("), "{three}");
        assert!(three.contains("'pr-3.example.com'"), "{three}");
        assert!(!three.contains("more"), "{three}");

        let five = message(5);
        assert!(five.contains("5 records conflict ("), "{five}");
        assert!(
            five.contains("'pr-3.example.com': no marker; and 2 more)"),
            "{five}"
        );
        assert!(!five.contains("'pr-4.example.com'"), "{five}");
    }

    #[test]
    fn hostname_decision_rejected_message_names_the_decision_and_record() {
        let message = DnsError::HostnameDecisionRejected {
            zone: "example.com".into(),
            name: "pr-1.example.com".into(),
            record_type: "A".into(),
            decision: "adopt",
            reason: "the record changed after the preview".into(),
        }
        .to_string();
        assert!(
            message.contains(
                "The adopt decision for A record 'pr-1.example.com' in zone 'example.com' no longer applies: the record changed after the preview."
            ),
            "{message}"
        );
        assert!(
            message.contains("preview the hostname mode again"),
            "{message}"
        );
    }

    #[test]
    fn unfinalized_marker_message_says_whether_the_record_stays_managed() {
        let message = |stays_managed| {
            DnsError::ManagedRecordMarkerNotFinalized(Box::new(MarkerNotFinalized {
                zone: "example.com".into(),
                name: "app".into(),
                record_type: "A".into(),
                stays_managed,
                proxied: false,
                source: DnsError::ApiError("simulated provider outage".into()),
            }))
            .to_string()
        };
        let managed = message(true);
        assert!(
            managed.contains("A record 'app' in zone example.com was written"),
            "{managed}"
        );
        assert!(
            managed
                .contains("stays managed by this install and its next write finalizes the marker"),
            "{managed}"
        );
        assert!(managed.ends_with("simulated provider outage"), "{managed}");
        let unmanaged = message(false);
        assert!(
            unmanaged.contains("no longer recognized as managed"),
            "{unmanaged}"
        );
    }

    #[test]
    fn incomplete_hostname_mode_message_names_changes_and_recovery() {
        let change = |index: usize| RecordChange {
            action: "create".into(),
            name: format!("h{index}.example.com"),
            record_type: "A".into(),
            value: "192.0.2.10".into(),
        };
        let message = |completed: Vec<RecordChange>, saved| {
            DnsError::HostnameModeIncomplete(Box::new(HostnameModeIncomplete {
                provider_id: 7,
                zone: "example.com".into(),
                mode: "flat".into(),
                completed,
                saved,
                source: DnsError::ApiError("simulated provider outage".into()),
            }))
            .to_string()
        };
        let early = message(Vec::new(), HostnameModeSaved::RecordStates);
        assert!(
            early.contains("Applying hostname mode 'flat' to zone 'example.com' on DNS provider 7 stopped after changing no DNS record"),
            "{early}"
        );
        assert!(early.contains("hostname mode was not changed"), "{early}");
        assert!(
            early.contains("with their record states saved for origin certificates"),
            "{early}"
        );
        assert!(
            early.contains("apply the previous mode to undo them"),
            "{early}"
        );

        // Never claims record states were saved when saving them failed.
        let unsaved = message(vec![change(0)], HostnameModeSaved::Nothing);
        assert!(
            unsaved.contains("hostname mode was not changed"),
            "{unsaved}"
        );
        assert!(
            unsaved.contains("saving their record states failed too"),
            "{unsaved}"
        );
        assert!(!unsaved.contains("states saved"), "{unsaved}");

        let late = message((0..12).map(change).collect(), HostnameModeSaved::Mode);
        assert!(
            late.contains("12 DNS change(s): create A h0.example.com"),
            "{late}"
        );
        assert!(
            late.contains("create A h9.example.com, and 2 more"),
            "{late}"
        );
        assert!(!late.contains("h10.example.com"), "{late}");
        assert!(late.contains("record states were saved"), "{late}");
        assert!(late.ends_with("simulated provider outage"), "{late}");
    }
}
