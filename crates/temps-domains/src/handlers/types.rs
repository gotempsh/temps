use crate::{CertificateRepository, DomainService, TlsService};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use utoipa::ToSchema;

pub struct DomainAppState {
    pub tls_service: Arc<TlsService>,
    pub repository: Arc<dyn CertificateRepository>,
    pub domain_service: Arc<DomainService>,
}

pub fn create_domain_app_state(
    tls_service: Arc<TlsService>,
    repository: Arc<dyn CertificateRepository>,
    domain_service: Arc<DomainService>,
) -> Arc<DomainAppState> {
    Arc::new(DomainAppState {
        tls_service,
        repository,
        domain_service,
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
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ListDomainsResponse {
    pub domains: Vec<DomainResponse>,
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
