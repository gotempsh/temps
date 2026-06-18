use crate::{CertificateRepository, DomainService, TlsService};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::AuditLogger;
use temps_dns::services::DnsProviderService;

use utoipa::ToSchema;

pub struct DomainAppState {
    pub tls_service: Arc<TlsService>,
    pub repository: Arc<dyn CertificateRepository>,
    pub domain_service: Arc<DomainService>,
    /// DNS provider service for automatic DNS record setup (optional)
    pub dns_provider_service: Option<Arc<DnsProviderService>>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
}

pub fn create_domain_app_state(
    tls_service: Arc<TlsService>,
    repository: Arc<dyn CertificateRepository>,
    domain_service: Arc<DomainService>,
    audit_service: Arc<dyn AuditLogger>,
    telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
) -> Arc<DomainAppState> {
    Arc::new(DomainAppState {
        tls_service,
        repository,
        domain_service,
        dns_provider_service: None,
        audit_service,
        telemetry,
    })
}

pub fn create_domain_app_state_with_dns(
    tls_service: Arc<TlsService>,
    repository: Arc<dyn CertificateRepository>,
    domain_service: Arc<DomainService>,
    dns_provider_service: Arc<DnsProviderService>,
    audit_service: Arc<dyn AuditLogger>,
    telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
) -> Arc<DomainAppState> {
    Arc::new(DomainAppState {
        tls_service,
        repository,
        domain_service,
        dns_provider_service: Some(dns_provider_service),
        audit_service,
        telemetry,
    })
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CreateDomainRequest {
    pub domain: String,
    /// Challenge type for Let's Encrypt validation. Options: "http-01" (default) or "dns-01"
    #[serde(default = "default_challenge_type")]
    pub challenge_type: String,
}

fn default_challenge_type() -> String {
    "http-01".to_string()
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DomainResponse {
    pub id: i32,
    pub domain: String,
    pub status: String,
    pub expiration_time: Option<i64>,
    pub last_renewed: Option<i64>,
    pub dns_challenge_token: Option<String>,
    pub dns_challenge_value: Option<String>,
    pub last_error: Option<String>,
    pub last_error_type: Option<String>,
    pub is_wildcard: bool,
    pub verification_method: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// The PEM-encoded certificate chain (can be displayed in browser or downloaded)
    pub certificate: Option<String>,
    /// On-demand TLS negative-cache deadline (epoch millis), when this hostname's
    /// on-demand HTTP-01 issuance is in backoff after a failure (ADR-018 §4).
    /// `None` means no active backoff.
    pub on_demand_backoff_until: Option<i64>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DomainChallengeResponse {
    pub domain: String,
    /// Array of TXT records to add to DNS. For wildcards, multiple records are required.
    pub txt_records: Vec<TxtRecord>,
    pub status: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DnsCompletionResponse {
    pub domain: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TxtRecord {
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DomainError {
    pub message: String,
    pub code: String,
    pub details: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(tag = "type")]
pub enum ProvisionResponse {
    #[serde(rename = "error")]
    Error(DomainError),
    #[serde(rename = "complete")]
    Complete(DomainResponse),
    #[serde(rename = "pending")]
    Pending(DomainChallengeResponse),
}

impl From<temps_entities::domains::Model> for DomainResponse {
    fn from(domain: temps_entities::domains::Model) -> Self {
        Self {
            id: domain.id,
            domain: domain.domain,
            status: domain.status,
            expiration_time: domain.expiration_time.map(|dt| dt.timestamp_millis()),
            last_renewed: domain.last_renewed.map(|dt| dt.timestamp_millis()),
            dns_challenge_token: domain.dns_challenge_token,
            dns_challenge_value: domain.dns_challenge_value,
            last_error: domain.last_error,
            last_error_type: domain.last_error_type,
            is_wildcard: domain.is_wildcard,
            verification_method: domain.verification_method,
            created_at: domain.created_at.timestamp_millis(),
            updated_at: domain.updated_at.timestamp_millis(),
            certificate: domain.certificate,
            on_demand_backoff_until: domain
                .on_demand_backoff_until
                .map(|dt| dt.timestamp_millis()),
        }
    }
}

impl From<crate::tls::models::Certificate> for DomainResponse {
    fn from(cert: crate::tls::models::Certificate) -> Self {
        use crate::tls::models::CertificateStatus;

        let status = match cert.status {
            CertificateStatus::Active => "active".to_string(),
            CertificateStatus::Pending => "pending".to_string(),
            CertificateStatus::PendingDns => "pending_dns".to_string(),
            CertificateStatus::PendingValidation => "pending_validation".to_string(),
            CertificateStatus::Failed {
                error: _,
                error_type: _,
            } => "failed".to_string(),
            CertificateStatus::Expired => "expired".to_string(),
        };

        let (last_error, last_error_type) = match cert.status {
            CertificateStatus::Failed { error, error_type } => (Some(error), Some(error_type)),
            _ => (None, None),
        };

        Self {
            id: 0, // Certificate model doesn't have ID, will need to get from database
            domain: cert.domain.clone(),
            status,
            expiration_time: Some(cert.expiration_time.timestamp_millis()),
            last_renewed: cert.last_renewed.map(|dt| dt.timestamp_millis()),
            dns_challenge_token: None, // Will be populated by challenge methods
            dns_challenge_value: None, // Will be populated by challenge methods
            last_error,
            last_error_type,
            is_wildcard: cert.is_wildcard,
            verification_method: cert.verification_method,
            created_at: chrono::Utc::now().timestamp_millis(),
            updated_at: chrono::Utc::now().timestamp_millis(),
            certificate: Some(cert.certificate_pem),
            // The `Certificate` model carries no on-demand backoff state; that
            // lives on the `domains` row, surfaced via the other `From` impl.
            on_demand_backoff_until: None,
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ListDomainsResponse {
    pub domains: Vec<DomainResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct AcmeOrderResponse {
    pub id: i32,
    pub order_url: String,
    pub domain_id: i32,
    pub email: String,
    pub status: String,
    pub identifiers: serde_json::Value,
    pub authorizations: Option<serde_json::Value>,
    pub finalize_url: Option<String>,
    pub certificate_url: Option<String>,
    pub error: Option<String>,
    pub error_type: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
    /// Live challenge validation status fetched from Let's Encrypt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge_validation: Option<ChallengeValidationStatus>,
}

#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct ChallengeValidationStatus {
    /// Challenge type (e.g., "dns-01", "http-01")
    #[serde(rename = "type")]
    pub challenge_type: String,
    /// Challenge validation URL
    pub url: String,
    /// Challenge status (e.g., "pending", "valid", "invalid")
    pub status: String,
    /// When the challenge was validated (if successful)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validated: Option<String>,
    /// Error details if validation failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ChallengeError>,
    /// Challenge token
    pub token: String,
}

#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct ChallengeError {
    /// Error type (e.g., "urn:ietf:params:acme:error:unauthorized")
    #[serde(rename = "type")]
    pub error_type: String,
    /// Human-readable error description
    pub detail: String,
    /// HTTP status code
    pub status: i32,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ListOrdersResponse {
    pub orders: Vec<AcmeOrderResponse>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct HttpChallengeDebugResponse {
    pub domain: String,
    pub challenge_exists: bool,
    pub challenge_token: Option<String>,
    /// The full URL that Let's Encrypt will try to access to validate the challenge
    pub challenge_url: Option<String>,
    /// The ACME validation URL (internal to ACME protocol)
    pub validation_url: Option<String>,
    /// IPv4 addresses the domain points to
    pub dns_a_records: Vec<String>,
    /// IPv6 addresses the domain points to
    pub dns_aaaa_records: Vec<String>,
    /// Any DNS resolution errors
    pub dns_error: Option<String>,
}

impl From<crate::tls::service::HttpChallengeDebugInfo> for HttpChallengeDebugResponse {
    fn from(info: crate::tls::service::HttpChallengeDebugInfo) -> Self {
        Self {
            domain: info.domain,
            challenge_exists: info.challenge_exists,
            challenge_token: info.challenge_token,
            challenge_url: info.challenge_url,
            validation_url: info.validation_url,
            dns_a_records: info.dns_a_records,
            dns_aaaa_records: info.dns_aaaa_records,
            dns_error: info.dns_error,
        }
    }
}

impl From<crate::tls::models::AcmeOrder> for AcmeOrderResponse {
    fn from(order: crate::tls::models::AcmeOrder) -> Self {
        Self {
            id: order.id,
            order_url: order.order_url,
            domain_id: order.domain_id,
            email: order.email,
            status: order.status,
            identifiers: order.identifiers,
            authorizations: order.authorizations,
            finalize_url: order.finalize_url,
            certificate_url: order.certificate_url,
            error: order.error,
            error_type: order.error_type,
            created_at: order.created_at.timestamp_millis(),
            updated_at: order.updated_at.timestamp_millis(),
            expires_at: order.expires_at.map(|dt| dt.timestamp_millis()),
            challenge_validation: None, // Will be populated by fetching from Let's Encrypt
        }
    }
}

// ========================================
// DNS Challenge Auto-Provisioning Types
// ========================================

/// Request to setup DNS challenge records using a configured DNS provider
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetupDnsChallengeRequest {
    /// The ID of the DNS provider to use for creating the TXT records
    pub dns_provider_id: i32,
}

/// Result of a single DNS TXT record creation for ACME challenge
#[derive(Debug, Serialize, ToSchema)]
pub struct DnsChallengeRecordResult {
    /// TXT record name (e.g., "_acme-challenge.example.com")
    #[schema(example = "_acme-challenge.example.com")]
    pub name: String,
    /// TXT record value (the ACME challenge token)
    #[schema(example = "abc123...")]
    pub value: String,
    /// Whether the record was created successfully
    pub success: bool,
    /// Human-readable message about the operation
    pub message: String,
}

/// Response from DNS challenge setup operation
#[derive(Debug, Serialize, ToSchema)]
pub struct SetupDnsChallengeResponse {
    /// Overall success status (true if all records were created)
    pub success: bool,
    /// Number of TXT records that were successfully created
    pub records_created: u32,
    /// Total number of TXT records required for the challenge
    pub total_records: u32,
    /// Results for each individual TXT record
    pub results: Vec<DnsChallengeRecordResult>,
    /// Human-readable summary message
    pub message: String,
}

// ========================================
// On-demand TLS observability types (ADR-018 §5)
// ========================================

/// A single on-demand HTTP-01 issuance attempt from the append-only
/// `on_demand_cert_attempts` audit log. Carries the full forensic detail for one
/// attempt; the current cert state lives on the enclosing row's domain fields.
///
/// Contains no private-key or certificate material — only audit metadata — so it
/// is safe to return without masking.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OnDemandCertAttemptResponse {
    pub id: i32,
    /// SNI hostname that triggered the attempt.
    pub hostname: String,
    /// What triggered the attempt (always `"tls_callback"` today).
    pub trigger: String,
    /// Did the proxy serve the `/.well-known/acme-challenge/` request?
    pub challenge_served: Option<bool>,
    /// Did we reach the Let's Encrypt API?
    pub acme_request_sent: Option<bool>,
    /// HTTP status or ACME error type returned by Let's Encrypt, when known.
    pub acme_response_status: Option<String>,
    /// Final outcome: `"issued"`, `"failed"`, `"skipped_duplicate"`,
    /// `"skipped_gate"`, `"skipped_rate_limit"`, or `"skipped_no_route"`.
    pub outcome: String,
    /// Full `Display` chain of the error (all `source()` levels), when failed.
    pub error_chain: Option<String>,
    /// Coarse error category for UI labelling: `"rate_limited"`, `"dns_failure"`,
    /// `"acme_order_expired"`, `"challenge_mismatch"`, `"timeout"`, `"internal"`.
    pub error_category: Option<String>,
    /// End-to-end issuance duration in milliseconds (0/None for skipped).
    pub duration_ms: Option<i32>,
    /// When the attempt was recorded (epoch millis).
    pub created_at: i64,
}

impl From<temps_entities::on_demand_cert_attempts::Model> for OnDemandCertAttemptResponse {
    fn from(m: temps_entities::on_demand_cert_attempts::Model) -> Self {
        Self {
            id: m.id,
            hostname: m.hostname,
            trigger: m.trigger,
            challenge_served: m.challenge_served,
            acme_request_sent: m.acme_request_sent,
            acme_response_status: m.acme_response_status,
            outcome: m.outcome,
            error_chain: m.error_chain,
            error_category: m.error_category,
            duration_ms: m.duration_ms,
            created_at: m.created_at.timestamp_millis(),
        }
    }
}

/// One row of the on-demand certificates list: the most-recent attempt for a
/// hostname plus the current authoritative cert state from its `domains` row.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OnDemandCertRow {
    /// SNI hostname.
    pub hostname: String,
    /// Current cert lifecycle status from the `domains` row, when one exists:
    /// `on_demand_pending`, `on_demand_issuing`, `active`, `on_demand_failed`,
    /// etc. `None` when no `domains` row exists yet for this hostname.
    pub status: Option<String>,
    /// Certificate expiration (epoch millis), when an active cert exists.
    pub expiration_time: Option<i64>,
    /// On-demand negative-cache deadline (epoch millis), when in backoff.
    pub backoff_until: Option<i64>,
    /// The audit record for the attempt this row represents (newest first in
    /// the list).
    pub attempt: OnDemandCertAttemptResponse,
}

/// Paginated list of on-demand cert attempts (ADR-018 §5 console "Certificates"
/// surface). Joined with current `domains.status`, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ListOnDemandCertsResponse {
    pub certs: Vec<OnDemandCertRow>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// Current on-demand cert status for a single hostname (ADR-018 §5). Backs
/// `GET /domains/by-host/{hostname}/cert-status`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CertStatusResponse {
    /// SNI hostname.
    pub hostname: String,
    /// Current cert lifecycle status from the `domains` row, when one exists.
    pub status: Option<String>,
    /// On-demand negative-cache deadline (epoch millis), when in backoff.
    pub backoff_until: Option<i64>,
    /// The most recent on-demand issuance attempt for this hostname, if any.
    pub last_attempt: Option<OnDemandCertAttemptResponse>,
}
