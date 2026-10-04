// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for DNS provider management
//!
//! This module contains the API endpoints for managing DNS providers,
//! managed domains, and DNS records.
//!
//! The `dns_sync` submodule contains a separate, internal-only API
//! consumed by per-node DNS resolvers (ADR-011) — it has a different auth
//! model, a different consumer, and lives behind its own
//! [`dns_sync::DnsSyncAppState`].

pub mod dns_sync;
pub mod domain_delivery;
pub mod managed_records;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_check, Permission, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditOperation, ForceRouteReloadJob, Job, RequestMetadata};
use utoipa::{OpenApi, ToSchema};

use crate::errors::{DnsError, HostnameModeSaved};
use crate::providers::{
    AzureCredentials, BunnyCredentials, CloudflareCredentials, DigitalOceanCredentials,
    DnsProviderType, DnsRecord, DnsZone, GcpCredentials, NamecheapCredentials, PebbleCredentials,
    ProviderCredentials, Route53Credentials,
};
use crate::services::hostname_sync::{
    AdoptRecordDecision, ConflictDecisions, HostnameModeResult, SkipRecordDecision,
};
use crate::services::{
    AddManagedDomainRequest, CreateProviderRequest, DnsProviderService, DnsRecordService,
    UpdateManagedDomainRequest, UpdateProviderRequest,
};

/// Audit record for managed-domain write operations.
#[derive(Debug, Clone, serde::Serialize)]
struct DnsGovernanceAudit {
    context: AuditContext,
    provider_id: i32,
    domain: String,
    action: String,
    details: serde_json::Value,
}

impl AuditOperation for DnsGovernanceAudit {
    fn operation_type(&self) -> String {
        self.action.clone()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

impl From<HostnameModeResult> for HostnamePreviewResponse {
    fn from(r: HostnameModeResult) -> Self {
        let hostname_changes: Vec<HostnameChange> = r
            .hostname_changes
            .into_iter()
            .map(|c| HostnameChange {
                kind: c.kind,
                id: c.id,
                old: c.old,
                new: c.new,
            })
            .collect();
        let dns_changes: Vec<DnsRecordChange> = r
            .dns_changes
            .into_iter()
            .map(|c| DnsRecordChange {
                action: c.action,
                name: c.name,
                record_type: c.record_type,
                value: c.value,
            })
            .collect();
        let conflicts: Vec<DnsRecordConflict> = r
            .conflicts
            .into_iter()
            .map(|c| DnsRecordConflict {
                name: c.name,
                record_type: c.record_type,
                value: c.value,
                proxied: c.proxied,
                reason: c.reason,
                adoptable: c.adoptable,
                current_value: c.current_value,
                current_proxied: c.current_proxied,
                revision: c.revision,
            })
            .collect();
        let total = hostname_changes.len() + dns_changes.len();
        HostnamePreviewResponse {
            hostname_changes,
            dns_changes,
            conflicts,
            zone_access_ok: r.zone_access_ok,
            total,
        }
    }
}

/// Application state for DNS handlers
pub struct DnsAppState {
    pub provider_service: Arc<DnsProviderService>,
    pub record_service: Arc<DnsRecordService>,
    pub managed_record_service: Arc<crate::services::ManagedDnsRecordService>,
    pub domain_delivery_service: Arc<crate::services::domain_delivery::DomainDeliveryService>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// Queue used to trigger a route reload after a hostname-mode change so
    /// derived (Standard/Flat) hostnames take effect.
    pub queue: Arc<dyn temps_core::JobQueue>,
    /// Audit logger for write operations.
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}

// ========================================
// Request/Response Types
// ========================================

/// Request to create a new DNS provider
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateDnsProviderRequest {
    /// User-friendly name
    #[schema(example = "My Cloudflare")]
    pub name: String,
    /// Provider type
    pub provider_type: DnsProviderType,
    /// Provider credentials
    pub credentials: DnsProviderCredentials,
    /// Optional description
    pub description: Option<String>,
}

/// Request to update a DNS provider
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateDnsProviderRequest {
    /// New name
    pub name: Option<String>,
    /// New credentials
    pub credentials: Option<DnsProviderCredentials>,
    /// New description
    pub description: Option<String>,
    /// Active status
    pub is_active: Option<bool>,
}

/// DNS provider credentials (API-facing)
#[derive(Clone, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DnsProviderCredentials {
    Bunny {
        api_key: String,
    },
    Cloudflare {
        #[schema(example = "your-api-token")]
        api_token: String,
        account_id: Option<String>,
    },
    Namecheap {
        #[schema(example = "your-username")]
        api_user: String,
        #[schema(example = "your-api-key")]
        api_key: String,
        client_ip: Option<String>,
        #[serde(default)]
        sandbox: bool,
    },
    Route53 {
        #[schema(example = "AKIAIOSFODNN7EXAMPLE")]
        access_key_id: String,
        #[schema(example = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")]
        secret_access_key: String,
        session_token: Option<String>,
        #[schema(example = "us-east-1")]
        region: Option<String>,
    },
    #[serde(rename = "digitalocean")]
    DigitalOcean {
        #[schema(example = "dop_v1_your-token")]
        api_token: String,
    },
    Gcp {
        #[schema(example = "dns-admin@myproject.iam.gserviceaccount.com")]
        service_account_email: String,
        #[schema(example = "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----")]
        private_key: String,
        #[schema(example = "my-gcp-project")]
        project_id: String,
    },
    Azure {
        #[schema(example = "00000000-0000-0000-0000-000000000000")]
        tenant_id: String,
        #[schema(example = "00000000-0000-0000-0000-000000000000")]
        client_id: String,
        client_secret: String,
        #[schema(example = "00000000-0000-0000-0000-000000000000")]
        subscription_id: String,
        #[schema(example = "my-resource-group")]
        resource_group: String,
    },
    /// Pebble challtestsrv mock DNS (LOCAL DEV/TEST ONLY)
    Pebble {
        #[schema(example = "http://localhost:8055")]
        management_url: String,
    },
}

impl std::fmt::Debug for DnsProviderCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DnsProviderCredentials([REDACTED])")
    }
}

impl From<DnsProviderCredentials> for ProviderCredentials {
    fn from(creds: DnsProviderCredentials) -> Self {
        match creds {
            DnsProviderCredentials::Bunny { api_key } => {
                ProviderCredentials::Bunny(BunnyCredentials { api_key })
            }
            DnsProviderCredentials::Cloudflare {
                api_token,
                account_id,
            } => ProviderCredentials::Cloudflare(CloudflareCredentials {
                api_token,
                account_id,
            }),
            DnsProviderCredentials::Namecheap {
                api_user,
                api_key,
                client_ip,
                sandbox,
            } => ProviderCredentials::Namecheap(NamecheapCredentials {
                api_user,
                api_key,
                client_ip,
                sandbox,
            }),
            DnsProviderCredentials::Route53 {
                access_key_id,
                secret_access_key,
                session_token,
                region,
            } => ProviderCredentials::Route53(Route53Credentials {
                access_key_id,
                secret_access_key,
                session_token,
                region,
            }),
            DnsProviderCredentials::DigitalOcean { api_token } => {
                ProviderCredentials::DigitalOcean(DigitalOceanCredentials { api_token })
            }
            DnsProviderCredentials::Gcp {
                service_account_email,
                private_key,
                project_id,
            } => ProviderCredentials::Gcp(GcpCredentials {
                service_account_email,
                private_key,
                project_id,
            }),
            DnsProviderCredentials::Azure {
                tenant_id,
                client_id,
                client_secret,
                subscription_id,
                resource_group,
            } => ProviderCredentials::Azure(AzureCredentials {
                tenant_id,
                client_id,
                client_secret,
                subscription_id,
                resource_group,
            }),
            DnsProviderCredentials::Pebble { management_url } => {
                ProviderCredentials::Pebble(PebbleCredentials { management_url })
            }
        }
    }
}

/// DNS provider response
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DnsProviderResponse {
    pub id: i32,
    pub name: String,
    pub provider_type: String,
    /// Masked credentials for display
    pub credentials: serde_json::Value,
    pub is_active: bool,
    pub description: Option<String>,
    pub last_used_at: Option<String>,
    pub last_error: Option<String>,
    /// Whether this provider benefits from the flat hostname mode (e.g. Cloudflare
    /// Universal SSL). The UI surfaces/recommends the Flat toggle when true.
    pub flat_hostnames_supported: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to add a managed domain
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AddManagedDomainApiRequest {
    #[schema(example = "example.com")]
    pub domain: String,
    #[serde(default = "default_true")]
    pub auto_manage: bool,
    #[serde(default)]
    pub proxied_by_default: bool,
    /// Generated hostname layout: `"standard"` (default) or `"flat"`.
    #[serde(default)]
    pub generated_hostname_mode: Option<String>,
    /// Opt in to reconciling generated hostnames into this domain's DNS zone.
    #[serde(default)]
    pub sync_generated_records: bool,
}

fn default_true() -> bool {
    true
}

fn managed_domain_automation_enabled(auto_manage: bool, sync_generated_records: bool) -> bool {
    auto_manage || sync_generated_records
}

/// Request to update a managed domain's settings.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateManagedDomainApiRequest {
    /// `"standard"` or `"flat"`. Persisted as-is; switching to `"flat"` does not
    /// recompute existing hostnames — use the apply endpoint for that.
    pub generated_hostname_mode: Option<String>,
    /// Toggle DNS record sync for this domain.
    pub sync_generated_records: Option<bool>,
    /// Toggle automatic DNS management for this domain.
    pub auto_manage: Option<bool>,
    /// Default proxy mode for newly managed records; `false` is an explicit override.
    pub proxied_by_default: Option<bool>,
}

/// Request to apply a hostname mode (recompute + optional DNS sync).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ApplyHostnameModeRequest {
    /// Target mode to apply: `"standard"` or `"flat"`.
    pub mode: String,
    /// Also reconcile the provider's DNS zone for the affected hostnames.
    #[serde(default)]
    pub sync_dns: bool,
    /// Conflicting records, from the preview's `conflicts`, that the user
    /// confirmed the sync may adopt — one entry per record, each only where
    /// the conflict is `adoptable`. Requires `sync_dns`.
    #[serde(default)]
    pub adopt_records: Vec<AdoptHostnameRecord>,
    /// Conflicting hostnames, from the preview's `conflicts`, that the user
    /// chose to leave untouched: the sync writes everything else. Requires
    /// `sync_dns`.
    #[serde(default)]
    pub skip_records: Vec<SkipHostnameRecord>,
}

/// A conflicting record the user confirmed the generated-hostname sync may
/// adopt: stamp it as the sync's own record, then point it at the value the
/// sync writes. The apply refuses it when the conflict changed after the
/// preview, so only the record the user reviewed is ever adopted.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdoptHostnameRecord {
    /// The conflict's `name`.
    #[schema(example = "pr-12.preview.example.com")]
    pub name: String,
    /// The conflict's `record_type`.
    #[schema(example = "A")]
    pub record_type: String,
    /// The conflict's `revision`, from the preview the user reviewed.
    pub revision: String,
}

/// A conflicting generated hostname the user chose to leave untouched. The
/// apply refuses it when the conflict changed after the preview, so a skip
/// never covers a record state the user did not review.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SkipHostnameRecord {
    /// The conflict's `name`.
    #[schema(example = "pr-12.preview.example.com")]
    pub name: String,
    /// The conflict's `record_type`.
    #[schema(example = "A")]
    pub record_type: String,
    /// The conflict's `revision`, from the preview the user reviewed.
    pub revision: String,
}

impl ApplyHostnameModeRequest {
    fn conflict_decisions(&self) -> ConflictDecisions {
        ConflictDecisions {
            adopt: self
                .adopt_records
                .iter()
                .map(|record| AdoptRecordDecision {
                    name: record.name.clone(),
                    record_type: record.record_type.clone(),
                    revision: record.revision.clone(),
                })
                .collect(),
            skip: self
                .skip_records
                .iter()
                .map(|record| SkipRecordDecision {
                    name: record.name.clone(),
                    record_type: record.record_type.clone(),
                    revision: record.revision.clone(),
                })
                .collect(),
        }
    }
}

/// Managed domain response
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagedDomainResponse {
    pub id: i32,
    pub provider_id: i32,
    pub domain: String,
    pub zone_id: Option<String>,
    pub auto_manage: bool,
    pub proxied_by_default: bool,
    pub verified: bool,
    pub verified_at: Option<String>,
    pub verification_error: Option<String>,
    /// Generated hostname layout: `"standard"` or `"flat"`.
    pub generated_hostname_mode: String,
    /// Whether generated hostnames are reconciled into the provider's DNS zone.
    pub sync_generated_records: bool,
    /// Last token zone-access check: `Some(true)`/`Some(false)`/`None` (unchecked).
    pub zone_access_ok: Option<bool>,
    /// Detail for a failed zone-access check.
    pub zone_access_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<temps_entities::dns_managed_domains::Model> for ManagedDomainResponse {
    fn from(d: temps_entities::dns_managed_domains::Model) -> Self {
        Self {
            id: d.id,
            provider_id: d.provider_id,
            domain: d.domain,
            zone_id: d.zone_id,
            auto_manage: d.auto_manage,
            proxied_by_default: d.proxied_by_default,
            verified: d.verified,
            verified_at: d.verified_at.map(|t| t.to_rfc3339()),
            verification_error: d.verification_error,
            generated_hostname_mode: d.generated_hostname_mode,
            sync_generated_records: d.sync_generated_records,
            zone_access_ok: d.zone_access_ok,
            zone_access_error: d.zone_access_error,
            created_at: d.created_at.to_rfc3339(),
            updated_at: d.updated_at.to_rfc3339(),
        }
    }
}

/// A single generated-hostname change in a flatten preview/apply.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostnameChange {
    /// `"deployment"` or `"environment"`.
    pub kind: String,
    /// Row id of the affected record.
    pub id: i32,
    pub old: String,
    pub new: String,
}

/// A single DNS record change the generated-hostname sync would make, or
/// made.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DnsRecordChange {
    /// `"create"`, `"update"` or `"delete"`; `"adopt"` for a record the user
    /// confirmed adopting (`value` is its value before any update);
    /// `"skip"` for a hostname the user chose to leave untouched;
    /// `"conflict"` for one nobody decided on yet (see `conflicts`), which
    /// makes an apply change nothing; `"restore"` for a record written back
    /// after its replacement failed.
    pub action: String,
    pub name: String,
    /// Record type, e.g. `"A"` or `"CNAME"`.
    pub record_type: String,
    pub value: String,
}

/// A generated hostname whose record the sync may not write without the
/// user's decision: adopt the record at its name (when `adoptable`), or skip
/// the hostname.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DnsRecordConflict {
    /// Fully-qualified generated hostname.
    #[schema(example = "pr-12.preview.example.com")]
    pub name: String,
    /// Record type the sync publishes the hostname as.
    #[schema(example = "A")]
    pub record_type: String,
    /// Value the sync would write.
    #[schema(example = "203.0.113.10")]
    pub value: String,
    /// Whether the sync would write the record proxied.
    pub proxied: bool,
    /// Why the sync may not write the record, and what resolves it.
    pub reason: String,
    /// Whether the record at this name can be adopted. Records another Temps
    /// workflow or installation owns, and ambiguous states, cannot: skip
    /// them, or resolve them at the provider and preview again.
    pub adoptable: bool,
    /// Value of the record at this name and type, when there is exactly one.
    #[schema(example = "203.0.113.10")]
    pub current_value: Option<String>,
    /// Whether that record is proxied.
    pub current_proxied: Option<bool>,
    /// Identifies what this preview showed about the conflict. Send it with
    /// the adopt or skip decision: the apply refuses a decision whose
    /// conflict changed after the preview (the record's value, proxy status
    /// or owner, a record next to it, or the value the sync would write).
    #[schema(example = "9f2c4e1ab3d5f6071829304a5b6c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8")]
    pub revision: String,
}

/// Combined preview of a hostname-mode change.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostnamePreviewResponse {
    pub hostname_changes: Vec<HostnameChange>,
    pub dns_changes: Vec<DnsRecordChange>,
    /// Generated hostnames whose records the sync may not write until the
    /// apply adopts or skips each one. Only a preview reports them: an apply
    /// with any left unresolved changes nothing and fails.
    pub conflicts: Vec<DnsRecordConflict>,
    /// Whether the provider token can manage this zone (None if not checked).
    pub zone_access_ok: Option<bool>,
    pub total: usize,
}

/// Connection test result
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectionTestResult {
    pub success: bool,
    pub message: String,
}

/// Zone list response
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ZoneListResponse {
    pub zones: Vec<DnsZone>,
}

/// Record list response
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecordListResponse {
    pub records: Vec<DnsRecord>,
}

// ========================================
// Error Handling
// ========================================

impl From<DnsError> for Problem {
    fn from(error: DnsError) -> Self {
        match error {
            DnsError::ProviderNotFound(id) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Provider Not Found")
                .with_detail(format!("DNS provider with ID {} not found", id)),
            DnsError::ProviderInactive {
                provider_id,
                provider_name,
            } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("DNS Provider Is Inactive")
                .with_detail(format!(
                    "DNS provider {} ({}) is inactive and cannot perform this operation",
                    provider_id, provider_name
                )),
            DnsError::DomainNotFound(domain) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Domain Not Found")
                .with_detail(format!("Domain {} not found", domain)),
            DnsError::DeliveryProfileNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Delivery Profile Not Found")
                .with_detail(error.to_string()),
            DnsError::ManagedDomainAlreadyExists { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Managed DNS Domain Already Exists")
                    .with_detail(error.to_string())
            }
            DnsError::AmbiguousManagedDomain { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Ambiguous Managed DNS Zone")
                .with_detail(error.to_string()),
            DnsError::ZoneNotFound(zone) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Zone Not Found")
                .with_detail(format!("DNS zone {} not found", zone)),
            DnsError::RecordNotFound(record) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Record Not Found")
                .with_detail(format!("DNS record {} not found", record)),
            DnsError::InvalidProviderType(t) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Provider Type")
                .with_detail(format!("Unknown provider type: {}", t)),
            DnsError::InvalidCredentials(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Credentials")
                .with_detail(msg),
            DnsError::Validation(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(msg),
            DnsError::PermissionDenied(msg) => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Permission Denied")
                .with_detail(msg),
            DnsError::RateLimited(msg) => problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                .with_title("Rate Limited")
                .with_detail(msg),
            DnsError::NotSupported(msg) => problemdetails::new(StatusCode::NOT_IMPLEMENTED)
                .with_title("Not Supported")
                .with_detail(msg),
            DnsError::ApiError(msg) => problemdetails::new(StatusCode::BAD_GATEWAY)
                .with_title("API Error")
                .with_detail(msg),
            DnsError::DomainNotManaged(_) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Domain Not Managed")
                .with_detail(error.to_string()),
            DnsError::RecordConflict { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("DNS Record Conflict")
                .with_detail(error.to_string()),
            DnsError::ResourceInUse { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Resource In Use")
                .with_detail(error.to_string()),
            DnsError::NotOwnedByInstance { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("DNS Record Owned By Another Instance")
                .with_detail(error.to_string()),
            DnsError::OwnedByOtherScope(_) => problemdetails::new(StatusCode::CONFLICT)
                .with_title("DNS Record Owned By Another Workflow")
                .with_detail(error.to_string()),
            DnsError::RecordLocked { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("DNS Record Busy")
                .with_detail(error.to_string()),
            DnsError::ZoneOperationInProgress { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("DNS Zone Busy")
                .with_detail(error.to_string()),
            DnsError::GeneratedHostnameConflicts { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Generated Hostname Conflicts")
                    .with_detail(error.to_string())
            }
            DnsError::HostnameDecisionRejected { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Conflict Decision No Longer Applies")
                .with_detail(error.to_string()),
            DnsError::ProxiedDepthUnsupported { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Proxied Record Too Deep")
                    .with_detail(error.to_string())
            }
            DnsError::ProxyNotSupportedByProvider { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Proxying Not Supported")
                    .with_detail(error.to_string())
            }
            DnsError::ConnectionFailed(_) | DnsError::Request(_) => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("DNS Provider Unreachable")
                    .with_detail(error.to_string())
            }
            DnsError::Encryption(_)
            | DnsError::Decryption(_)
            | DnsError::Database(_)
            | DnsError::Serialization(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Error")
                .with_detail(error.to_string()),
            DnsError::DeliveryZoneUnavailable { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Delivery Zone Unavailable")
                .with_detail(error.to_string()),
            // A row that is gone is 404; a row that still exists but is
            // fenced for deletion conflicts with the apply, so 409.
            DnsError::DeliveryProjectNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(error.to_string()),
            DnsError::DeliveryProjectBeingDeleted { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Project Is Being Deleted")
                    .with_detail(error.to_string())
            }
            DnsError::DeliveryEnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            DnsError::DeliveryEnvironmentDeleted { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Environment Deleted")
                    .with_detail(error.to_string())
            }
            // Keep the status and title of the error that stopped the
            // operation, so clients see the code they always did; the detail
            // also names the steps that had already completed.
            DnsError::DeliveryIncomplete(incomplete) => {
                let detail = incomplete.to_string();
                Problem::from(incomplete.source).with_detail(detail)
            }
            DnsError::ManagedRecordMarkerNotFinalized(incomplete) => {
                let detail = incomplete.to_string();
                Problem::from(incomplete.source).with_detail(detail)
            }
            DnsError::HostnameModeIncomplete(incomplete) => {
                let detail = incomplete.to_string();
                Problem::from(incomplete.source).with_detail(detail)
            }
        }
    }
}

// ========================================
// Handlers
// ========================================

/// List all DNS providers
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers",
    responses(
        (status = 200, description = "List of DNS providers", body = Vec<DnsProviderResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_dns_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersRead);

    let providers = state.provider_service.list().await?;

    let responses: Vec<DnsProviderResponse> = providers
        .into_iter()
        .map(|p| {
            let masked_creds = state
                .provider_service
                .get_masked_credentials(&p)
                .unwrap_or_else(|_| serde_json::json!({}));
            let flat_supported = state.provider_service.flat_hostnames_supported(&p);

            DnsProviderResponse {
                id: p.id,
                name: p.name,
                provider_type: p.provider_type,
                credentials: masked_creds,
                is_active: p.is_active,
                description: p.description,
                last_used_at: p.last_used_at.map(|t| t.to_rfc3339()),
                last_error: p.last_error,
                flat_hostnames_supported: flat_supported,
                created_at: p.created_at.to_rfc3339(),
                updated_at: p.updated_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(responses))
}

/// Create a new DNS provider
///
/// The provider's credentials will be tested before creation.
/// If the connection test fails, the provider will not be created.
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers",
    request_body = CreateDnsProviderRequest,
    responses(
        (status = 201, description = "DNS provider created", body = DnsProviderResponse),
        (status = 400, description = "Invalid request or connection test failed"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn create_dns_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateDnsProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);

    let credentials: ProviderCredentials = request.credentials.into();

    // Test the credentials before creating the provider
    state
        .provider_service
        .test_credentials(&request.provider_type, &credentials)
        .await?;

    // Credentials are valid, create the provider
    let provider = state
        .provider_service
        .create(CreateProviderRequest {
            name: request.name,
            provider_type: request.provider_type,
            credentials,
            description: request.description,
        })
        .await?;

    let masked_creds = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));
    let flat_supported = state.provider_service.flat_hostnames_supported(&provider);

    let response = DnsProviderResponse {
        id: provider.id,
        name: provider.name.clone(),
        provider_type: provider.provider_type.clone(),
        credentials: masked_creds,
        is_active: provider.is_active,
        description: provider.description,
        last_used_at: provider.last_used_at.map(|t| t.to_rfc3339()),
        last_error: provider.last_error,
        flat_hostnames_supported: flat_supported,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        provider.id,
        "",
        "DNS_PROVIDER_CREATED",
        serde_json::json!({
            "provider_name": provider.name,
            "provider_type": provider.provider_type,
        }),
    )
    .await;

    Ok((StatusCode::CREATED, Json(response)))
}

/// Get a DNS provider by ID
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers/{id}",
    responses(
        (status = 200, description = "DNS provider details", body = DnsProviderResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_dns_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersRead);

    let provider = state.provider_service.get(id).await?;

    let masked_creds = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));
    let flat_supported = state.provider_service.flat_hostnames_supported(&provider);

    let response = DnsProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: provider.provider_type,
        credentials: masked_creds,
        is_active: provider.is_active,
        description: provider.description,
        last_used_at: provider.last_used_at.map(|t| t.to_rfc3339()),
        last_error: provider.last_error,
        flat_hostnames_supported: flat_supported,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    Ok(Json(response))
}

/// Update a DNS provider
///
/// If new credentials are supplied, they are tested before the update is
/// persisted (same as creation) -- otherwise a provider's credentials (and,
/// for Pebble, its target URL) could be swapped for something invalid or
/// unsafe without ever going through validation.
#[utoipa::path(
    tag = "DNS Providers",
    put,
    path = "/dns-providers/{id}",
    request_body = UpdateDnsProviderRequest,
    responses(
        (status = 200, description = "DNS provider updated", body = DnsProviderResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
        (status = 409, description = "Deactivation refused while domain delivery bindings use the provider", body = temps_core::problemdetails::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn update_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateDnsProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);

    let changed_fields = serde_json::json!({
        "name": request.name.is_some(),
        "credentials": request.credentials.is_some(),
        "description": request.description.is_some(),
        "is_active": request.is_active,
    });
    let credentials: Option<ProviderCredentials> = request.credentials.map(|c| c.into());

    if let Some(credentials) = &credentials {
        let existing = state.provider_service.get(id).await?;
        let provider_type = DnsProviderType::from_str(&existing.provider_type)?;
        state
            .provider_service
            .test_credentials(&provider_type, credentials)
            .await?;
    }

    let provider = state
        .provider_service
        .update(
            id,
            UpdateProviderRequest {
                name: request.name,
                credentials,
                description: request.description,
                is_active: request.is_active,
            },
        )
        .await?;

    let masked_creds = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));
    let flat_supported = state.provider_service.flat_hostnames_supported(&provider);

    let response = DnsProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: provider.provider_type,
        credentials: masked_creds,
        is_active: provider.is_active,
        description: provider.description,
        last_used_at: provider.last_used_at.map(|t| t.to_rfc3339()),
        last_error: provider.last_error,
        flat_hostnames_supported: flat_supported,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        provider.id,
        "",
        "DNS_PROVIDER_UPDATED",
        changed_fields,
    )
    .await;

    Ok(Json(response))
}

/// Delete a DNS provider
#[utoipa::path(
    tag = "DNS Providers",
    delete,
    path = "/dns-providers/{id}",
    responses(
        (status = 204, description = "DNS provider deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
        (status = 409, description = "Provider is still used by domain delivery bindings", body = temps_core::problemdetails::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn delete_dns_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);

    let provider = state.provider_service.get(id).await?;
    state.provider_service.delete(id).await?;
    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        id,
        "",
        "DNS_PROVIDER_DELETED",
        serde_json::json!({
            "provider_name": provider.name,
            "provider_type": provider.provider_type,
        }),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// Test provider connection
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers/{id}/test",
    responses(
        (status = 200, description = "Connection test result", body = ConnectionTestResult),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn test_provider_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);

    let success = state.provider_service.test_connection(id).await?;
    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        id,
        "",
        "DNS_PROVIDER_CONNECTION_TESTED",
        serde_json::json!({ "success": success }),
    )
    .await;

    let response = ConnectionTestResult {
        success,
        message: if success {
            "Connection successful".to_string()
        } else {
            "Connection failed".to_string()
        },
    };

    Ok(Json(response))
}

/// List zones available in a provider
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers/{id}/zones",
    responses(
        (status = 200, description = "List of zones", body = ZoneListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_provider_zones(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersRead);

    let provider = state.provider_service.get(id).await?;
    let instance = state.provider_service.create_provider_instance(&provider)?;

    let zones = instance.list_zones().await?;

    Ok(Json(ZoneListResponse { zones }))
}

/// Add a managed domain to a provider
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers/{id}/domains",
    request_body = AddManagedDomainApiRequest,
    responses(
        (status = 201, description = "Managed domain added", body = ManagedDomainResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn add_managed_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<AddManagedDomainApiRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    // Same rule as `update_managed_domain`: enabling Cloudflare proxying by
    // default changes how traffic reaches every generated record, so it is a
    // DNS-automation decision, not a plain provider write.
    if managed_domain_automation_enabled(request.auto_manage, request.sync_generated_records)
        || request.proxied_by_default
    {
        permission_check!(auth, Permission::DnsAutomationWrite);
    }

    let managed = state
        .provider_service
        .add_managed_domain(
            id,
            AddManagedDomainRequest {
                domain: request.domain,
                auto_manage: request.auto_manage,
                proxied_by_default: request.proxied_by_default,
                generated_hostname_mode: request.generated_hostname_mode,
                sync_generated_records: request.sync_generated_records,
            },
        )
        .await?;

    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        id,
        &managed.domain,
        "DNS_MANAGED_DOMAIN_ADDED",
        serde_json::json!({
            "auto_manage": managed.auto_manage,
            "proxied_by_default": managed.proxied_by_default,
            "sync_generated_records": managed.sync_generated_records,
            "verified": managed.verified,
        }),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(ManagedDomainResponse::from(managed)),
    ))
}

/// List managed domains for a provider
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers/{id}/domains",
    responses(
        (status = 200, description = "List of managed domains", body = Vec<ManagedDomainResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_managed_domains(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersRead);

    let domains = state.provider_service.list_managed_domains(id).await?;

    let responses: Vec<ManagedDomainResponse> = domains
        .into_iter()
        .map(ManagedDomainResponse::from)
        .collect();

    Ok(Json(responses))
}

/// Remove a managed domain
#[utoipa::path(
    tag = "DNS Providers",
    delete,
    path = "/dns-providers/{provider_id}/domains/{domain}",
    responses(
        (status = 204, description = "Managed domain removed"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 409, description = "Managed domain is still used by domain delivery bindings", body = temps_core::problemdetails::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn remove_managed_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((provider_id, domain)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);

    state
        .provider_service
        .remove_managed_domain(provider_id, &domain)
        .await?;
    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        provider_id,
        &domain,
        "DNS_MANAGED_DOMAIN_REMOVED",
        serde_json::json!({}),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// Verify a managed domain
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers/{provider_id}/domains/{domain}/verify",
    responses(
        (status = 200, description = "Domain verification result", body = ManagedDomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn verify_managed_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((provider_id, domain)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);

    let _verified = state
        .provider_service
        .verify_managed_domain(provider_id, &domain)
        .await?;

    // Fetch the updated domain
    let domains = state
        .provider_service
        .list_managed_domains(provider_id)
        .await?;
    let managed = domains
        .into_iter()
        .find(|d| d.domain == domain)
        .ok_or_else(|| DnsError::DomainNotFound(domain))?;
    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        provider_id,
        &managed.domain,
        "DNS_MANAGED_DOMAIN_VERIFIED",
        serde_json::json!({ "verified": managed.verified }),
    )
    .await;

    Ok(Json(ManagedDomainResponse::from(managed)))
}

/// Update a managed domain's settings (hostname mode, sync opt-in, auto-manage).
#[utoipa::path(
    tag = "DNS Providers",
    patch,
    path = "/dns-providers/{provider_id}/domains/{domain}",
    request_body = UpdateManagedDomainApiRequest,
    responses(
        (status = 200, description = "Managed domain updated", body = ManagedDomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 409, description = "Turning off automatic management refused while domain delivery bindings use the zone, or a hostname-mode change refused while another generated-hostname operation runs on the zone (retryable)", body = temps_core::problemdetails::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn update_managed_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((provider_id, domain)): Path<(i32, String)>,
    Json(request): Json<UpdateManagedDomainApiRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    let existing = state
        .provider_service
        .get_managed_domain(provider_id, &domain)
        .await?;
    if managed_domain_automation_enabled(
        request.auto_manage.unwrap_or(existing.auto_manage),
        request
            .sync_generated_records
            .unwrap_or(existing.sync_generated_records),
    ) || request.proxied_by_default.is_some()
    {
        permission_check!(auth, Permission::DnsAutomationWrite);
    }

    let changes = serde_json::json!({
        "generated_hostname_mode": request.generated_hostname_mode,
        "sync_generated_records": request.sync_generated_records,
        "auto_manage": request.auto_manage,
        "proxied_by_default": request.proxied_by_default,
    });

    let updated = state
        .provider_service
        .update_managed_domain(
            provider_id,
            &domain,
            UpdateManagedDomainRequest {
                generated_hostname_mode: request.generated_hostname_mode,
                sync_generated_records: request.sync_generated_records,
                auto_manage: request.auto_manage,
                proxied_by_default: request.proxied_by_default,
            },
        )
        .await?;

    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        provider_id,
        &domain,
        "DNS_MANAGED_DOMAIN_UPDATED",
        changes,
    )
    .await;

    Ok(Json(ManagedDomainResponse::from(updated)))
}

/// Query parameters for the hostname-mode preview.
#[derive(Debug, Clone, Deserialize)]
struct HostnamePreviewQuery {
    /// Target mode: `"standard"` or `"flat"`.
    mode: String,
    /// Whether to include the DNS record changes the sync would make.
    #[serde(default)]
    sync: bool,
}

/// Preview the impact of switching a managed domain's hostname mode.
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers/{provider_id}/domains/{domain}/hostname-preview",
    params(
        ("mode" = String, Query, description = "Target mode: standard|flat"),
        ("sync" = Option<bool>, Query, description = "Include DNS record changes"),
    ),
    responses(
        (status = 200, description = "Hostname mode preview", body = HostnamePreviewResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn preview_hostname_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path((provider_id, domain)): Path<(i32, String)>,
    Query(query): Query<HostnamePreviewQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersRead);

    let target = DnsProviderService::parse_requested_hostname_mode(&query.mode)?;
    let result = state
        .provider_service
        .preview_hostname_mode(provider_id, &domain, target, query.sync)
        .await?;

    Ok(Json(HostnamePreviewResponse::from(result)))
}

/// Apply a hostname mode to a managed domain (persist + optional DNS sync +
/// route reload).
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers/{provider_id}/domains/{domain}/apply-hostname-mode",
    request_body = ApplyHostnameModeRequest,
    responses(
        (status = 200, description = "Hostname mode applied", body = HostnamePreviewResponse),
        (status = 400, description = "Invalid mode, or adopt/skip decisions that are duplicated, name a record type the sync never publishes, or were sent without sync_dns", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions or token lacks zone access"),
        (status = 404, description = "Domain not found"),
        (status = 409, description = "Nothing was changed: a generated hostname's record conflicts and no decision adopts or skips it, an adopt or skip decision no longer matches the zone (preview again), or another generated-hostname operation is running on the zone (retry when it completes)", body = temps_core::problemdetails::ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
async fn apply_hostname_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((provider_id, domain)): Path<(i32, String)>,
    Json(request): Json<ApplyHostnameModeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    if request.sync_dns {
        permission_check!(auth, Permission::DnsAutomationWrite);
    }

    let target = DnsProviderService::parse_requested_hostname_mode(&request.mode)?;
    let outcome = state
        .provider_service
        .apply_hostname_mode(
            provider_id,
            &domain,
            target,
            request.sync_dns,
            &request.conflict_decisions(),
            auth.user_id(),
        )
        .await;
    let result = match outcome {
        Ok(result) => result,
        Err(error) => {
            // A failed apply may already have changed DNS records, and even
            // the hostname mode, so it is audited with what it changed.
            log_dns_governance_audit(
                &state,
                &auth,
                &metadata,
                provider_id,
                &domain,
                "DNS_HOSTNAME_MODE_APPLY_FAILED",
                hostname_mode_failure_audit_details(&request, &error),
            )
            .await;
            // An apply that saved the new mode before it failed changed which
            // hostnames are generated, so routes must follow the saved mode.
            // The request still fails with its own error.
            if failed_apply_saved_mode(&error) {
                if let Err(reload_error) = queue_route_reload(&state).await {
                    tracing::error!(
                        "Failed to enqueue route reload after hostname mode '{}' was saved for {} (DNS provider {}) by an apply that then failed: {}",
                        request.mode,
                        domain,
                        provider_id,
                        reload_error
                    );
                }
            }
            return Err(error.into());
        }
    };

    // The DNS and settings writes are durable at this point, so audit them
    // before anything below can fail the request.
    log_dns_governance_audit(
        &state,
        &auth,
        &metadata,
        provider_id,
        &domain,
        "DNS_HOSTNAME_MODE_APPLIED",
        serde_json::json!({
            "mode": request.mode,
            "sync_dns": request.sync_dns,
            "outcome": "succeeded",
            "dns_change_count": result.dns_changes.len(),
            "adopted_records": request.adopt_records,
            "skipped_records": request.skip_records,
        }),
    )
    .await;

    // Trigger a full route reload so derived (Standard/Flat) hostnames take
    // effect. Never report a fully successful apply when the route plane was
    // not notified; the durable reconciliation run preserves what DNS changed.
    if let Err(e) = queue_route_reload(&state).await {
        tracing::error!(
            "Failed to enqueue route reload after hostname mode change: {}",
            e
        );
        return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Route Reload Failed")
            .with_detail(format!(
                "DNS and hostname settings were applied, but the route reload could not be queued: {e}"
            )));
    }

    Ok(Json(HostnamePreviewResponse::from(result)))
}

/// Queue a full route reload, so derived (Standard/Flat) hostnames follow the
/// saved hostname mode.
async fn queue_route_reload(state: &DnsAppState) -> Result<(), temps_core::QueueError> {
    state
        .queue
        .send(Job::ForceRouteReload(ForceRouteReloadJob {
            environment_id: None,
            deployment_id: None,
        }))
        .await
}

/// What a failed apply saved before it returned: nothing unless it had
/// already changed DNS records.
fn failed_apply_saved(error: &DnsError) -> HostnameModeSaved {
    match error {
        DnsError::HostnameModeIncomplete(incomplete) => incomplete.saved,
        _ => HostnameModeSaved::Nothing,
    }
}

/// Whether a failed apply had already saved the new hostname mode, so the
/// generated hostnames, and the routes serving them, have changed.
fn failed_apply_saved_mode(error: &DnsError) -> bool {
    failed_apply_saved(error) == HostnameModeSaved::Mode
}

/// Audit details of a failed hostname-mode apply: the adopt and skip
/// decisions it was sent, what it saved (the mode, only the record states of
/// its changes, or nothing), every DNS change it made before failing (none
/// when it failed before changing anything), and the error returned to the
/// caller.
fn hostname_mode_failure_audit_details(
    request: &ApplyHostnameModeRequest,
    error: &DnsError,
) -> serde_json::Value {
    let completed_changes = match error {
        DnsError::HostnameModeIncomplete(incomplete) => incomplete.completed.as_slice(),
        _ => &[],
    };
    serde_json::json!({
        "mode": request.mode,
        "sync_dns": request.sync_dns,
        "outcome": "failed",
        "adopted_records": request.adopt_records,
        "skipped_records": request.skip_records,
        "saved": failed_apply_saved(error),
        "completed_changes": completed_changes,
        "error": error.to_string(),
    })
}

async fn log_dns_governance_audit(
    state: &Arc<DnsAppState>,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    provider_id: i32,
    domain: &str,
    action: &str,
    details: serde_json::Value,
) {
    let audit = DnsGovernanceAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        provider_id,
        domain: domain.to_string(),
        action: action.to_string(),
        details,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!("Failed to create audit log: {}", e);
    }
}

// ========================================
// Router Configuration
// ========================================

/// Configure DNS routes
pub fn configure_routes() -> Router<Arc<DnsAppState>> {
    Router::new()
        // Provider management
        .route(
            "/dns-providers",
            get(list_dns_providers).post(create_dns_provider),
        )
        .route(
            "/dns-providers/{id}",
            get(get_dns_provider)
                .put(update_provider)
                .delete(delete_dns_provider),
        )
        .route("/dns-providers/{id}/test", post(test_provider_connection))
        .route("/dns-providers/{id}/zones", get(list_provider_zones))
        // Managed domains
        .route(
            "/dns-providers/{id}/domains",
            get(list_managed_domains).post(add_managed_domain),
        )
        .route(
            "/dns-providers/{provider_id}/domains/{domain}",
            delete(remove_managed_domain).patch(update_managed_domain),
        )
        .route(
            "/dns-providers/{provider_id}/domains/{domain}/verify",
            post(verify_managed_domain),
        )
        .route(
            "/dns-providers/{provider_id}/domains/{domain}/hostname-preview",
            get(preview_hostname_mode),
        )
        .route(
            "/dns-providers/{provider_id}/domains/{domain}/apply-hostname-mode",
            post(apply_hostname_mode),
        )
        // Ownership-guarded managed records (ADR-031)
        .route(
            "/dns-records",
            post(managed_records::set_managed_record)
                .delete(managed_records::remove_managed_record),
        )
        .route(
            "/dns-records/ownership",
            get(managed_records::get_record_ownership),
        )
        .route(
            "/dns-records/import",
            post(managed_records::import_managed_record),
        )
        .route(
            "/delivery-capabilities",
            get(domain_delivery::get_delivery_capabilities),
        )
        .route(
            "/delivery-profiles",
            get(domain_delivery::list_delivery_profiles)
                .post(domain_delivery::create_delivery_profile),
        )
        .route(
            "/delivery-profiles/{profile_id}",
            get(domain_delivery::get_delivery_profile)
                .delete(domain_delivery::delete_delivery_profile),
        )
        .route(
            "/projects/{project_id}/delivery-settings",
            get(domain_delivery::get_project_delivery_settings)
                .put(domain_delivery::update_project_delivery_settings),
        )
        .route(
            "/projects/{project_id}/domain-delivery-bindings",
            get(domain_delivery::list_domain_delivery_bindings),
        )
        .route(
            "/projects/{project_id}/domain-delivery-bindings/preview",
            post(domain_delivery::preview_domain_delivery_binding),
        )
        .route(
            "/projects/{project_id}/domain-delivery-bindings/apply",
            post(domain_delivery::apply_domain_delivery_binding),
        )
        .route(
            "/projects/{project_id}/domain-delivery-bindings/{binding_id}",
            delete(domain_delivery::delete_domain_delivery_binding),
        )
}

/// Configure internal DNS sync routes (ADR-011).
///
/// These are *not* user-facing — they're polled by the per-node Hickory
/// resolver running inside `temps-agent`. Auth is per-node bearer token,
/// not the user JWT used by [`configure_routes`].
pub fn configure_internal_routes() -> Router<Arc<dns_sync::DnsSyncAppState>> {
    Router::new()
        .route(
            "/internal/nodes/{node_id}/dns/changes",
            get(dns_sync::get_dns_changes),
        )
        .route(
            "/internal/nodes/{node_id}/dns/ack",
            post(dns_sync::post_dns_ack),
        )
}

// ========================================
// OpenAPI Documentation
// ========================================

#[derive(OpenApi)]
#[openapi(
    paths(
        list_dns_providers,
        create_dns_provider,
        get_dns_provider,
        update_provider,
        delete_dns_provider,
        test_provider_connection,
        list_provider_zones,
        add_managed_domain,
        list_managed_domains,
        remove_managed_domain,
        update_managed_domain,
        verify_managed_domain,
        preview_hostname_mode,
        apply_hostname_mode,
        managed_records::get_record_ownership,
        managed_records::set_managed_record,
        managed_records::remove_managed_record,
        managed_records::import_managed_record,
        domain_delivery::get_delivery_capabilities,
        domain_delivery::list_delivery_profiles,
        domain_delivery::get_delivery_profile,
        domain_delivery::create_delivery_profile,
        domain_delivery::delete_delivery_profile,
        domain_delivery::get_project_delivery_settings,
        domain_delivery::update_project_delivery_settings,
        domain_delivery::list_domain_delivery_bindings,
        domain_delivery::preview_domain_delivery_binding,
        domain_delivery::apply_domain_delivery_binding,
        domain_delivery::delete_domain_delivery_binding,
        dns_sync::get_dns_changes,
        dns_sync::post_dns_ack,
    ),
    components(
        schemas(
            CreateDnsProviderRequest,
            UpdateDnsProviderRequest,
            DnsProviderCredentials,
            DnsProviderResponse,
            AddManagedDomainApiRequest,
            UpdateManagedDomainApiRequest,
            ApplyHostnameModeRequest,
            AdoptHostnameRecord,
            SkipHostnameRecord,
            ManagedDomainResponse,
            HostnameChange,
            DnsRecordChange,
            DnsRecordConflict,
            HostnamePreviewResponse,
            managed_records::SetManagedRecordRequest,
            managed_records::ImportManagedRecordRequest,
            managed_records::RecordOwnershipResponse,
            managed_records::ImportManagedRecordResponse,
            domain_delivery::CreateDeliveryProfileRequest,
            domain_delivery::UpdateProjectDeliverySettingsRequest,
            domain_delivery::ApplyDomainDeliveryBindingRequest,
            crate::services::domain_delivery::DeliveryProviderKind,
            crate::services::domain_delivery::DeliveryCapabilityResponse,
            crate::services::domain_delivery::DeliveryProfileResponse,
            crate::services::domain_delivery::DeliveryProfilePage,
            crate::services::domain_delivery::EnvironmentDeliveryOverride,
            crate::services::domain_delivery::ProjectDeliverySettingsResponse,
            crate::services::domain_delivery::PreviewDomainDeliveryBindingRequest,
            crate::services::domain_delivery::AdoptDeliveryRecord,
            crate::services::domain_delivery::DeliveryRecordPlan,
            crate::services::domain_delivery::DeliveryRecordRequirement,
            crate::services::domain_delivery::DeliveryRequirements,
            crate::services::domain_delivery::OriginTlsPolicy,
            crate::services::domain_delivery::DeliveryRoutingPlan,
            crate::services::domain_delivery::DomainDeliveryPreviewResponse,
            crate::services::domain_delivery::DomainDeliveryBindingResponse,
            crate::services::domain_delivery::DomainDeliveryBindingPage,
            ConnectionTestResult,
            ZoneListResponse,
            RecordListResponse,
            DnsProviderType,
            DnsZone,
            DnsRecord,
            dns_sync::EndpointDto,
            dns_sync::DnsChangesResponse,
            dns_sync::DnsAckRequest,
            dns_sync::DnsAckResponse,
        )
    ),
    tags(
        (name = "DNS Providers", description = "DNS provider management endpoints"),
        (name = "DNS Records", description = "Ownership-guarded managed DNS records (ADR-031)"),
        (name = "Internal DNS", description = "Per-node DNS resolver sync (ADR-011)"),
    )
)]
pub struct DnsApiDoc;

#[cfg(test)]
mod tests {
    use super::managed_domain_automation_enabled;
    use crate::errors::DnsError;
    use axum::http::StatusCode;
    use temps_core::problemdetails::Problem;

    fn status(error: DnsError) -> StatusCode {
        Problem::from(error).status_code
    }

    #[test]
    fn ownership_errors_map_to_client_statuses() {
        assert_eq!(
            status(DnsError::RecordConflict {
                domain: "example.com".into(),
                name: "app".into(),
                record_type: "CNAME".into(),
                reason: "record exists and is not managed by temps".into(),
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(DnsError::ResourceInUse {
                resource: "DNS provider",
                id: 7,
                name: "primary".into(),
                reason: "2 domain delivery binding(s) still use it".into(),
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(DnsError::DeliveryProfileNotFound { profile_id: 9 }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(DnsError::NotOwnedByInstance {
                domain: "example.com".into(),
                name: "app".into(),
                record_type: "CNAME".into(),
                owner_instance: "other".into(),
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(DnsError::ProxiedDepthUnsupported {
                fqdn: "a.b.example.com".into(),
                levels: 2,
                flat_suggestion: "a-b.example.com".into(),
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(DnsError::ProxyNotSupportedByProvider {
                provider: "namecheap".into(),
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(DnsError::DomainNotManaged("example.com".into())),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(DnsError::ConnectionFailed("Bunny API timed out".into())),
            StatusCode::BAD_GATEWAY
        );
    }

    /// A delivery reservation that finds its project or environment row gone
    /// is a 404; one that finds it fenced for deletion is a 409. Each detail
    /// names the IDs and the hostname.
    #[test]
    fn delivery_scope_errors_map_to_not_found_or_conflict_naming_their_ids() {
        let cases = [
            (
                DnsError::DeliveryProjectNotFound {
                    project_id: 7,
                    hostname: "app.example.com".into(),
                },
                StatusCode::NOT_FOUND,
                "Project Not Found",
                "Project 7 not found",
            ),
            (
                DnsError::DeliveryProjectBeingDeleted {
                    project_id: 7,
                    hostname: "app.example.com".into(),
                },
                StatusCode::CONFLICT,
                "Project Is Being Deleted",
                "Project 7 is being deleted",
            ),
            (
                DnsError::DeliveryEnvironmentNotFound {
                    project_id: 7,
                    environment_id: 10,
                    hostname: "app.example.com".into(),
                },
                StatusCode::NOT_FOUND,
                "Environment Not Found",
                "Environment 10 not found in project 7",
            ),
            (
                DnsError::DeliveryEnvironmentDeleted {
                    project_id: 7,
                    environment_id: 10,
                    hostname: "app.example.com".into(),
                },
                StatusCode::CONFLICT,
                "Environment Deleted",
                "Environment 10 of project 7 was deleted",
            ),
        ];
        for (error, expected_status, expected_title, expected_detail) in cases {
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, expected_status, "{expected_detail}");
            assert_eq!(
                problem.body.get("title").and_then(|value| value.as_str()),
                Some(expected_title)
            );
            let detail = problem
                .body
                .get("detail")
                .and_then(|value| value.as_str())
                .expect("detail");
            assert!(detail.contains(expected_detail), "{detail}");
            assert!(detail.contains("'app.example.com'"), "{detail}");
        }
    }

    /// Errors that report partial progress keep the status and title of the
    /// error that stopped the operation; the detail adds what had changed.
    #[test]
    fn partial_progress_errors_keep_the_status_of_their_cause() {
        let cause = || DnsError::ApiError("simulated provider outage".into());
        let expected = Problem::from(cause());
        let incomplete = [
            DnsError::ManagedRecordMarkerNotFinalized(Box::new(
                crate::errors::MarkerNotFinalized {
                    zone: "example.com".into(),
                    name: "app".into(),
                    record_type: "A".into(),
                    stays_managed: true,
                    proxied: false,
                    source: cause(),
                },
            )),
            DnsError::HostnameModeIncomplete(Box::new(crate::errors::HostnameModeIncomplete {
                provider_id: 7,
                zone: "example.com".into(),
                mode: "flat".into(),
                completed: Vec::new(),
                saved: crate::errors::HostnameModeSaved::RecordStates,
                source: cause(),
            })),
        ];
        for error in incomplete {
            let message = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, expected.status_code, "{message}");
            assert_eq!(problem.body.get("title"), expected.body.get("title"));
            assert_eq!(
                problem.body.get("detail").and_then(|value| value.as_str()),
                Some(message.as_str())
            );
        }
    }

    #[test]
    fn failed_apply_audit_records_what_was_saved_and_the_completed_changes() {
        use super::{
            failed_apply_saved_mode, hostname_mode_failure_audit_details, ApplyHostnameModeRequest,
        };
        use crate::errors::HostnameModeSaved;
        let request = ApplyHostnameModeRequest {
            mode: "flat".into(),
            sync_dns: true,
            adopt_records: vec![super::AdoptHostnameRecord {
                name: "pr-1.example.com".into(),
                record_type: "A".into(),
                revision: "revision-1".into(),
            }],
            skip_records: vec![super::SkipHostnameRecord {
                name: "pr-2.example.com".into(),
                record_type: "A".into(),
                revision: "revision-2".into(),
            }],
        };
        let incomplete = |saved| {
            DnsError::HostnameModeIncomplete(Box::new(crate::errors::HostnameModeIncomplete {
                provider_id: 7,
                zone: "example.com".into(),
                mode: "flat".into(),
                completed: vec![crate::services::hostname_sync::RecordChange {
                    action: "create".into(),
                    name: "app".into(),
                    record_type: "A".into(),
                    value: "203.0.113.10".into(),
                }],
                saved,
                source: DnsError::ApiError("simulated provider outage".into()),
            }))
        };

        // Stopped after saving the mode: routes must follow it.
        let switched = incomplete(HostnameModeSaved::Mode);
        assert!(failed_apply_saved_mode(&switched));
        let details = hostname_mode_failure_audit_details(&request, &switched);
        assert_eq!(details["outcome"], "failed");
        assert_eq!(details["saved"], "mode");
        assert_eq!(
            details["completed_changes"],
            serde_json::json!([{
                "action": "create",
                "name": "app",
                "record_type": "A",
                "value": "203.0.113.10",
            }])
        );
        let error = details["error"].as_str().unwrap_or_default();
        assert!(error.contains("simulated provider outage"), "{error}");
        // The decisions the user confirmed are part of what was attempted.
        assert_eq!(
            details["adopted_records"],
            serde_json::json!([{
                "name": "pr-1.example.com",
                "record_type": "A",
                "revision": "revision-1",
            }])
        );
        assert_eq!(
            details["skipped_records"],
            serde_json::json!([{
                "name": "pr-2.example.com",
                "record_type": "A",
                "revision": "revision-2",
            }])
        );

        // Stopped before the switch: the mode, and so the routes, are
        // unchanged, whether or not the record states could be saved.
        for (saved, recorded) in [
            (HostnameModeSaved::RecordStates, "record_states"),
            (HostnameModeSaved::Nothing, "nothing"),
        ] {
            let stopped = incomplete(saved);
            assert!(!failed_apply_saved_mode(&stopped));
            let details = hostname_mode_failure_audit_details(&request, &stopped);
            assert_eq!(details["saved"], recorded);
            assert_eq!(details["completed_changes"][0]["name"], "app");
        }

        // Refused before any write.
        let refused = DnsError::Validation("zone does not govern the preview domain".into());
        assert!(!failed_apply_saved_mode(&refused));
        let details = hostname_mode_failure_audit_details(&request, &refused);
        assert_eq!(details["saved"], "nothing");
        assert_eq!(details["completed_changes"], serde_json::json!([]));
    }

    #[test]
    fn hostname_conflicts_and_stale_decisions_are_conflicts_with_their_own_detail() {
        let conflicts = DnsError::GeneratedHostnameConflicts {
            zone: "example.com".into(),
            conflicts: vec!["A 'pr-1.example.com': no ownership marker".into()],
        };
        let rejected = DnsError::HostnameDecisionRejected {
            zone: "example.com".into(),
            name: "pr-1.example.com".into(),
            record_type: "A".into(),
            decision: "adopt",
            reason: "the record changed after the preview and is now 198.51.100.9".into(),
        };
        for (error, title) in [
            (conflicts, "Generated Hostname Conflicts"),
            (rejected, "Conflict Decision No Longer Applies"),
        ] {
            let message = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, StatusCode::CONFLICT, "{message}");
            assert_eq!(
                problem.body.get("title").and_then(|value| value.as_str()),
                Some(title)
            );
            assert_eq!(
                problem.body.get("detail").and_then(|value| value.as_str()),
                Some(message.as_str())
            );
        }
    }

    #[test]
    fn apply_request_decisions_default_to_none_and_map_one_to_one() {
        use super::ApplyHostnameModeRequest;
        let bare: ApplyHostnameModeRequest =
            serde_json::from_value(serde_json::json!({ "mode": "flat", "sync_dns": true }))
                .expect("decisions are optional");
        assert!(bare.conflict_decisions().is_empty());

        let request: ApplyHostnameModeRequest = serde_json::from_value(serde_json::json!({
            "mode": "flat",
            "sync_dns": true,
            "adopt_records": [{
                "name": "pr-1.example.com",
                "record_type": "A",
                "revision": "revision-1",
            }],
            "skip_records": [{
                "name": "pr-2.example.com",
                "record_type": "CNAME",
                "revision": "revision-2",
            }],
        }))
        .expect("a request with decisions parses");
        let decisions = request.conflict_decisions();
        assert_eq!(decisions.adopt.len(), 1);
        assert_eq!(decisions.adopt[0].name, "pr-1.example.com");
        assert_eq!(decisions.adopt[0].record_type, "A");
        assert_eq!(decisions.adopt[0].revision, "revision-1");
        assert_eq!(decisions.skip.len(), 1);
        assert_eq!(decisions.skip[0].name, "pr-2.example.com");
        assert_eq!(decisions.skip[0].record_type, "CNAME");
        assert_eq!(decisions.skip[0].revision, "revision-2");

        // A decision without the revision of the conflict it was made on is
        // malformed, not a guess at what the user reviewed.
        let unreviewed = serde_json::from_value::<ApplyHostnameModeRequest>(serde_json::json!({
            "mode": "flat",
            "sync_dns": true,
            "skip_records": [{ "name": "pr-2.example.com", "record_type": "A" }],
        }));
        assert!(unreviewed.is_err());
    }

    #[test]
    fn busy_zone_is_a_retryable_conflict_naming_provider_and_zone() {
        let problem = Problem::from(DnsError::ZoneOperationInProgress {
            provider_id: 7,
            zone: "example.com".into(),
        });

        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let text = |field: &str| {
            problem
                .body
                .get(field)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(text("title"), "DNS Zone Busy");
        let detail = text("detail");
        assert!(detail.contains("zone 'example.com'"), "{detail}");
        assert!(detail.contains("DNS provider 7"), "{detail}");
        assert!(detail.contains("retry"), "{detail}");
    }

    #[test]
    fn generated_record_sync_is_an_automation_capability() {
        assert!(managed_domain_automation_enabled(false, true));
    }

    #[test]
    fn manual_domain_without_generated_sync_is_not_automation() {
        assert!(!managed_domain_automation_enabled(false, false));
    }
}
