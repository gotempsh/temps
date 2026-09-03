// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;

/// TLS certificate stored in the `domains` table.
///
/// ## Key material hygiene
///
/// `private_key_pem` is sensitive material. Two mechanisms enforce this rather
/// than relying on prose promises (ADR-041 §6):
///
/// - **Hand-written `Debug`** redacts both `private_key_pem` and
///   `certificate_pem`. A struct that cannot be printed with its secret cannot
///   leak it through a later `debug!("{:?}", cert)`.
/// - **`Serialize` is deliberately NOT derived.** Use `CertificateSummary` for
///   API responses. A type that is not serializable cannot be handed to
///   `Json(cert)` by a future handler.
///
/// The `Deserialize` derive is kept so the model can be built from test data
/// and from internal-only code paths that own the source of truth.
#[derive(Clone, Deserialize)]
pub struct Certificate {
    pub id: i32,
    pub domain: String,
    /// Full PEM chain (leaf first). Stored verbatim; served verbatim by the
    /// proxy. Not secret, but redacted in `Debug` to prevent accidental
    /// inclusion in log payloads that travel out-of-band.
    pub certificate_pem: String,
    /// Decrypted private key PEM. Secret — encrypted at rest by
    /// `CertificateRepository::save_certificate` via `EncryptionService`.
    /// Never serialized, never logged.
    pub private_key_pem: String,
    pub expiration_time: UtcDateTime,
    pub last_renewed: Option<UtcDateTime>,
    pub is_wildcard: bool,
    pub verification_method: String,
    pub status: CertificateStatus,
}

/// Hand-written `Debug` for `Certificate` that redacts secret fields.
///
/// This replaces the derive to make it structurally impossible to leak key
/// material via `debug!("{:?}", cert)` added anywhere in the codebase.
impl std::fmt::Debug for Certificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Certificate")
            .field("id", &self.id)
            .field("domain", &self.domain)
            .field("certificate_pem", &"[REDACTED]")
            .field("private_key_pem", &"[REDACTED]")
            .field("expiration_time", &self.expiration_time)
            .field("last_renewed", &self.last_renewed)
            .field("is_wildcard", &self.is_wildcard)
            .field("verification_method", &self.verification_method)
            .field("status", &self.status)
            .finish()
    }
}

/// Non-secret summary of a certificate for API responses.
///
/// Contains only derived, non-secret metadata — no PEM material, no private
/// key. Every endpoint added by ADR-041 that surfaces certificate information
/// returns this type, not `Certificate` directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateSummary {
    pub id: i32,
    pub domain: String,
    pub expiration_time: UtcDateTime,
    pub last_renewed: Option<UtcDateTime>,
    pub is_wildcard: bool,
    pub verification_method: String,
    pub status: CertificateStatus,
    pub days_remaining: i64,
}

impl From<&Certificate> for CertificateSummary {
    fn from(cert: &Certificate) -> Self {
        Self {
            id: cert.id,
            domain: cert.domain.clone(),
            expiration_time: cert.expiration_time,
            last_renewed: cert.last_renewed,
            is_wildcard: cert.is_wildcard,
            verification_method: cert.verification_method.clone(),
            status: cert.status.clone(),
            days_remaining: cert.days_until_expiry(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CertificateStatus {
    Active,
    Pending,
    PendingDns,
    PendingValidation,
    Failed { error: String, error_type: String },
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsChallengeData {
    pub domain: String,
    pub txt_record_name: String,
    pub txt_record_value: String,
    pub order_url: Option<String>,
    pub created_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpChallengeData {
    pub domain: String,
    pub token: String,
    pub key_authorization: String,
    pub validation_url: Option<String>,
    pub order_url: Option<String>,
    pub created_at: UtcDateTime,
}

#[derive(Debug, Clone, Default)]
pub struct CertificateFilter {
    pub status: Option<CertificateStatus>,
    pub expiring_within_days: Option<i32>,
    pub is_wildcard: Option<bool>,
    pub domain_pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcmeAccount {
    pub email: String,
    pub environment: String,
    pub credentials: String,
    pub created_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcmeOrder {
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
    pub token: Option<String>, // For fast HTTP-01 challenge lookups (indexed)
    pub key_authorization: Option<String>, // For fast HTTP-01 challenge lookups
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub expires_at: Option<UtcDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChallengeType {
    Http01,
    Dns01,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsTxtRecord {
    pub name: String,
    pub value: String,
    /// The ACME validation URL for this specific challenge
    pub validation_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeData {
    pub challenge_type: ChallengeType,
    pub domain: String,
    pub token: String,
    pub key_authorization: String,
    pub validation_url: Option<String>,
    /// Array of DNS TXT records to add. For wildcards, multiple records are required.
    pub dns_txt_records: Vec<DnsTxtRecord>,
    pub order_url: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ProvisioningResult {
    Certificate(Certificate),
    Challenge(ChallengeData),
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub is_valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ChallengeStrategy {
    Http01,
}

/// Certificate renewal tracking model
/// Keeps certificate Active while renewal is in progress
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateRenewal {
    pub id: i32,
    pub domain_id: i32,
    pub current_certificate_id: i32,
    pub challenge_type: String,
    pub txt_record_name: Option<String>,
    pub txt_record_value: Option<String>,
    pub token: Option<String>,
    pub key_authorization: Option<String>,
    pub order_url: Option<String>,
    pub status: RenewalStatus,
    pub error: Option<String>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub expires_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RenewalStatus {
    Pending,
    AwaitingDnsValidation,
    AwaitingHttpValidation,
    Validating,
    Completed,
    Failed,
}

/// Result of a certificate renewal request.
///
/// Uses `CertificateSummary` (not `Certificate`) for all embedded certificate
/// fields: `RenewalResult` is serialized for API responses, and `Certificate`
/// is deliberately not `Serialize` to prevent accidental key-material leakage
/// (ADR-041 §6).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RenewalResult {
    /// Certificate doesn't need renewal yet
    NotNeeded {
        days_remaining: i64,
        current_certificate: CertificateSummary,
    },

    /// HTTP-01 renewal completed automatically
    Completed {
        certificate: CertificateSummary,
        renewed_at: UtcDateTime,
    },

    /// DNS-01 renewal started, awaiting user DNS update
    /// CRITICAL: current_certificate remains Active
    AwaitingDnsValidation {
        current_certificate: CertificateSummary, // Still Active!
        challenge_data: DnsChallengeData,
        instructions: String,
        renewal_id: i32,
    },

    /// HTTP-01 challenge initiated, awaiting validation
    AwaitingHttpValidation {
        current_certificate: CertificateSummary, // Still Active!
        challenge_data: HttpChallengeData,
        instructions: String,
        renewal_id: i32,
    },
}

impl Certificate {
    pub fn is_expired(&self) -> bool {
        chrono::Utc::now() > self.expiration_time
    }

    pub fn days_until_expiry(&self) -> i64 {
        let now = chrono::Utc::now();
        (self.expiration_time - now).num_days()
    }

    pub fn needs_renewal(&self) -> bool {
        self.days_until_expiry() <= 30
    }
}

impl From<temps_entities::domains::Model> for Certificate {
    fn from(entity: temps_entities::domains::Model) -> Self {
        let status = match entity.status.as_str() {
            "active" => CertificateStatus::Active,
            "pending" => CertificateStatus::Pending,
            "pending_dns" => CertificateStatus::PendingDns,
            "pending_validation" => CertificateStatus::PendingValidation,
            "failed" => CertificateStatus::Failed {
                error: entity.last_error.unwrap_or_default(),
                error_type: entity.last_error_type.unwrap_or_default(),
            },
            "expired" => CertificateStatus::Expired,
            _ => CertificateStatus::Pending,
        };

        Certificate {
            id: entity.id,
            domain: entity.domain,
            certificate_pem: entity.certificate.unwrap_or_default(),
            private_key_pem: entity.private_key.unwrap_or_default(),
            expiration_time: entity.expiration_time.unwrap_or_else(chrono::Utc::now),
            last_renewed: entity.last_renewed,
            is_wildcard: entity.is_wildcard,
            verification_method: entity.verification_method,
            status,
        }
    }
}

impl From<&Certificate> for temps_entities::domains::ActiveModel {
    fn from(cert: &Certificate) -> Self {
        use sea_orm::Set;
        use temps_entities::domains;

        let (status, last_error, last_error_type) = match &cert.status {
            CertificateStatus::Active => ("active".to_string(), None, None),
            CertificateStatus::Pending => ("pending".to_string(), None, None),
            CertificateStatus::PendingDns => ("pending_dns".to_string(), None, None),
            CertificateStatus::PendingValidation => ("pending_validation".to_string(), None, None),
            CertificateStatus::Failed { error, error_type } => (
                "failed".to_string(),
                Some(error.clone()),
                Some(error_type.clone()),
            ),
            CertificateStatus::Expired => ("expired".to_string(), None, None),
        };

        domains::ActiveModel {
            domain: Set(cert.domain.clone()),
            certificate: Set(Some(cert.certificate_pem.clone())),
            private_key: Set(Some(cert.private_key_pem.clone())),
            expiration_time: Set(Some(cert.expiration_time)),
            last_renewed: Set(cert.last_renewed),
            status: Set(status),
            last_error: Set(last_error),
            last_error_type: Set(last_error_type),
            is_wildcard: Set(cert.is_wildcard),
            verification_method: Set(cert.verification_method.clone()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_certificate_expiry() {
        let mut cert = Certificate {
            id: 0,
            domain: "example.com".to_string(),
            certificate_pem: String::new(),
            private_key_pem: String::new(),
            expiration_time: chrono::Utc::now() + Duration::days(45),
            last_renewed: None,
            is_wildcard: false,
            verification_method: "tls-alpn-01".to_string(),
            status: CertificateStatus::Active,
        };

        assert!(!cert.is_expired());
        assert!(!cert.needs_renewal());
        assert_eq!(cert.days_until_expiry(), 44); // Roughly 44-45 days

        // Test needs renewal (< 30 days)
        cert.expiration_time = chrono::Utc::now() + Duration::days(25);
        assert!(cert.needs_renewal());

        // Test expired
        cert.expiration_time = chrono::Utc::now() - Duration::days(1);
        assert!(cert.is_expired());
    }
}

/// Report of certificate renewal operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewalReport {
    pub total_checked: usize,
    pub auto_renewed: Vec<String>,
    pub renewal_failed: Vec<RenewalFailure>,
    pub manual_action_needed: Vec<ManualRenewalNeeded>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewalFailure {
    pub domain: String,
    pub error: String,
    pub verification_method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualRenewalNeeded {
    pub domain: String,
    pub expires_at: UtcDateTime,
    pub days_remaining: i64,
}
