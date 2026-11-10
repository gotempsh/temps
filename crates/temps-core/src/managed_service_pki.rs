// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Certificate authority and server certificates for the databases Temps
//! creates and manages (ADR 093).
//!
//! This CA is deliberately **not** the per-cluster CA in [`crate::node_pki`].
//! The cluster CA signs worker leaves that carry both `serverAuth` and
//! `clientAuth` and that every node and the control plane trust as cluster
//! identities. A leaf signed for a database by the same CA would be accepted
//! wherever a worker identity is, and a worker leaf would verify as a
//! database server for every client that trusts the database CA. Two roots
//! keep the two classes of certificate unable to impersonate each other, and
//! let either root be rotated without touching the other.
//!
//! This module only produces PEM material and metadata. Persistence (the CA
//! key encrypted at rest with `EncryptionService`) and delivery into the
//! service's volume live with their callers.

use chrono::{DateTime, Duration, TimeZone, Utc};
use rand::RngExt;
use rcgen::string::Ia5String;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType, SerialNumber,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Subject common name of the managed-services CA.
pub const MANAGED_SERVICES_CA_COMMON_NAME: &str = "Temps Managed Services CA";

/// How long the managed-services CA is valid for.
pub const CA_VALIDITY_DAYS: i64 = 3650;

/// How long a managed service's server certificate is valid for.
pub const SERVER_CERTIFICATE_VALIDITY_DAYS: i64 = 365;

/// A server certificate is replaced once it has fewer than this many days
/// left, so a service is never left serving a certificate about to expire.
pub const SERVER_CERTIFICATE_RENEW_BEFORE_DAYS: i64 = 30;

/// Certificates are back-dated by this much so a client whose clock is a
/// little behind the control plane's does not reject a fresh certificate as
/// "not yet valid".
const CLOCK_SKEW_ALLOWANCE_MINUTES: i64 = 5;

/// Errors from managed-service CA and certificate operations.
#[derive(Debug, Error)]
pub enum ManagedPkiError {
    #[error("Failed to generate the key pair for {purpose}: {reason}")]
    KeyGeneration { purpose: String, reason: String },

    #[error("Failed to build the certificate for '{subject}': {reason}")]
    CertificateBuild { subject: String, reason: String },

    #[error("Subject alternative name '{value}' for '{subject}' is not valid: {reason}")]
    InvalidSubjectAltName {
        subject: String,
        value: String,
        reason: String,
    },

    #[error("Server certificate for '{subject}' needs at least one subject alternative name")]
    NoSubjectAltNames { subject: String },

    #[error("The managed-services CA could not be loaded ({context}): {reason}")]
    InvalidAuthority { context: String, reason: String },

    #[error("Failed to sign the server certificate for '{subject}': {reason}")]
    Signing { subject: String, reason: String },

    #[error("Failed to parse PEM ({context}): {reason}")]
    Pem { context: String, reason: String },

    #[error("Timestamp {timestamp} cannot be represented in a certificate: {reason}")]
    InvalidTimestamp { timestamp: String, reason: String },
}

/// A freshly generated managed-services CA. PEM-encoded.
#[derive(Clone)]
pub struct ManagedServicesCa {
    /// Public CA certificate: what clients add to their trust store.
    pub cert_pem: String,
    /// CA private key: SECRET. Stored encrypted at rest, never returned.
    pub key_pem: String,
    /// SHA-256 (lowercase hex) of the certificate's DER encoding.
    pub fingerprint_sha256: String,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

impl std::fmt::Debug for ManagedServicesCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedServicesCa")
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

/// A server certificate and its freshly generated key. PEM-encoded.
#[derive(Clone)]
pub struct ServerCertificate {
    pub cert_pem: String,
    /// Server private key: SECRET. Delivered into the service's volume only.
    pub key_pem: String,
    /// SHA-256 (lowercase hex) of the certificate's DER encoding.
    pub fingerprint_sha256: String,
    pub subject_alt_names: Vec<String>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

impl std::fmt::Debug for ServerCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerCertificate")
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .field("subject_alt_names", &self.subject_alt_names)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

/// Generate a new managed-services CA (ECDSA P-256).
///
/// The CA may only sign end-entity certificates (path length 0), so a leaf it
/// issued can never be used to mint further certificates.
pub fn generate_managed_services_ca(
    now: DateTime<Utc>,
) -> Result<ManagedServicesCa, ManagedPkiError> {
    let key = KeyPair::generate().map_err(|e| ManagedPkiError::KeyGeneration {
        purpose: "the managed-services CA".into(),
        reason: e.to_string(),
    })?;
    let subject = MANAGED_SERVICES_CA_COMMON_NAME;
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| {
        ManagedPkiError::CertificateBuild {
            subject: subject.into(),
            reason: e.to_string(),
        }
    })?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, subject);
    dn.push(DnType::OrganizationName, "Temps");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    params.serial_number = Some(random_serial());
    let (not_before, not_after) = validity_window(now, CA_VALIDITY_DAYS);
    params.not_before = to_offset(not_before)?;
    params.not_after = to_offset(not_after)?;

    let cert = params
        .self_signed(&key)
        .map_err(|e| ManagedPkiError::CertificateBuild {
            subject: subject.into(),
            reason: e.to_string(),
        })?;
    let cert_pem = cert.pem();
    Ok(ManagedServicesCa {
        fingerprint_sha256: certificate_fingerprint_sha256(&cert_pem)?,
        cert_pem,
        key_pem: key.serialize_pem(),
        not_before,
        not_after,
    })
}

/// Issue a server certificate for one managed service, signed by the
/// managed-services CA.
///
/// The leaf carries only `serverAuth`, never `clientAuth`: a database's key
/// must not double as a client identity anywhere. `subject_alt_names` are
/// classified as IP or DNS SANs; duplicates and blanks are dropped.
pub fn issue_server_certificate(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    common_name: &str,
    subject_alt_names: &[String],
    now: DateTime<Utc>,
) -> Result<ServerCertificate, ManagedPkiError> {
    let sans = normalize_subject_alt_names(subject_alt_names);
    if sans.is_empty() {
        return Err(ManagedPkiError::NoSubjectAltNames {
            subject: common_name.into(),
        });
    }
    let ca_key = KeyPair::from_pem(ca_key_pem).map_err(|e| ManagedPkiError::InvalidAuthority {
        context: "CA key".into(),
        reason: e.to_string(),
    })?;
    let issuer = Issuer::from_ca_cert_pem(ca_cert_pem, ca_key).map_err(|e| {
        ManagedPkiError::InvalidAuthority {
            context: "CA certificate".into(),
            reason: e.to_string(),
        }
    })?;

    let key = KeyPair::generate().map_err(|e| ManagedPkiError::KeyGeneration {
        purpose: format!("the server certificate of '{common_name}'"),
        reason: e.to_string(),
    })?;
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| {
        ManagedPkiError::CertificateBuild {
            subject: common_name.into(),
            reason: e.to_string(),
        }
    })?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    dn.push(DnType::OrganizationName, "Temps");
    params.distinguished_name = dn;
    params.is_ca = IsCa::ExplicitNoCa;
    params.use_authority_key_identifier_extension = true;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.serial_number = Some(random_serial());
    params.subject_alt_names = sans
        .iter()
        .map(|value| san_from_str(common_name, value))
        .collect::<Result<_, _>>()?;
    let (not_before, not_after) = validity_window(now, SERVER_CERTIFICATE_VALIDITY_DAYS);
    params.not_before = to_offset(not_before)?;
    params.not_after = to_offset(not_after)?;

    let cert = params
        .signed_by(&key, &issuer)
        .map_err(|e| ManagedPkiError::Signing {
            subject: common_name.into(),
            reason: e.to_string(),
        })?;
    let cert_pem = cert.pem();
    Ok(ServerCertificate {
        fingerprint_sha256: certificate_fingerprint_sha256(&cert_pem)?,
        cert_pem,
        key_pem: key.serialize_pem(),
        subject_alt_names: sans,
        not_before,
        not_after,
    })
}

/// Whether a certificate valid until `not_after` should be replaced now.
pub fn needs_renewal(not_after: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    not_after - now < Duration::days(SERVER_CERTIFICATE_RENEW_BEFORE_DAYS)
}

/// SHA-256 (lowercase hex) of the first certificate in `cert_pem`.
pub fn certificate_fingerprint_sha256(cert_pem: &str) -> Result<String, ManagedPkiError> {
    use rustls::pki_types::pem::PemObject;
    let der =
        rustls::pki_types::CertificateDer::from_pem_slice(cert_pem.as_bytes()).map_err(|e| {
            ManagedPkiError::Pem {
                context: "certificate".into(),
                reason: e.to_string(),
            }
        })?;
    Ok(hex::encode(Sha256::digest(der.as_ref())))
}

/// Trim, drop blanks and de-duplicate (case-insensitively, as TLS compares
/// names), keeping the first spelling and the caller's order.
pub fn normalize_subject_alt_names(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim().trim_end_matches('.');
        if value.is_empty() || out.iter().any(|seen| seen.eq_ignore_ascii_case(value)) {
            continue;
        }
        out.push(value.to_string());
    }
    out
}

fn san_from_str(subject: &str, value: &str) -> Result<SanType, ManagedPkiError> {
    if let Ok(ip) = value.parse::<std::net::IpAddr>() {
        return Ok(SanType::IpAddress(ip));
    }
    Ia5String::try_from(value.to_string())
        .map(SanType::DnsName)
        .map_err(|e| ManagedPkiError::InvalidSubjectAltName {
            subject: subject.into(),
            value: value.into(),
            reason: e.to_string(),
        })
}

/// A random, positive 128-bit serial number (RFC 5280 allows up to 20 bytes).
fn random_serial() -> SerialNumber {
    let mut bytes: [u8; 16] = rand::rng().random();
    // Clear the sign bit so the DER INTEGER stays positive.
    bytes[0] &= 0x7f;
    // A leading zero byte would be stripped by DER; keep the length stable.
    bytes[0] |= 0x01;
    SerialNumber::from_slice(&bytes)
}

/// The validity window starting at `now` (minus a clock-skew allowance),
/// truncated to whole seconds, as certificates store it.
fn validity_window(now: DateTime<Utc>, days: i64) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = now - Duration::minutes(CLOCK_SKEW_ALLOWANCE_MINUTES);
    let start = Utc
        .timestamp_opt(start.timestamp(), 0)
        .single()
        .unwrap_or(start);
    (start, start + Duration::days(days))
}

fn to_offset(at: DateTime<Utc>) -> Result<time::OffsetDateTime, ManagedPkiError> {
    time::OffsetDateTime::from_unix_timestamp(at.timestamp()).map_err(|e| {
        ManagedPkiError::InvalidTimestamp {
            timestamp: at.to_rfc3339(),
            reason: e.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::prelude::{FromDer, X509Certificate};

    fn der(pem: &str) -> Vec<u8> {
        use rustls::pki_types::pem::PemObject;
        rustls::pki_types::CertificateDer::from_pem_slice(pem.as_bytes())
            .unwrap()
            .as_ref()
            .to_vec()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap()
    }

    #[test]
    fn the_ca_is_a_path_length_zero_authority_valid_for_ten_years() {
        let ca = generate_managed_services_ca(now()).unwrap();
        assert!(ca.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.key_pem.contains("PRIVATE KEY"));
        assert_eq!(ca.fingerprint_sha256.len(), 64);
        assert_eq!(
            ca.not_after - ca.not_before,
            Duration::days(CA_VALIDITY_DAYS)
        );
        assert!(ca.not_before < now());

        let der = der(&ca.cert_pem);
        let (_, cert) = X509Certificate::from_der(&der).unwrap();
        let constraints = cert.basic_constraints().unwrap().unwrap().value;
        assert!(constraints.ca);
        assert_eq!(constraints.path_len_constraint, Some(0));
        assert_eq!(
            cert.subject()
                .iter_common_name()
                .next()
                .unwrap()
                .as_str()
                .unwrap(),
            MANAGED_SERVICES_CA_COMMON_NAME
        );
        assert_eq!(
            cert.validity().not_after.timestamp(),
            ca.not_after.timestamp()
        );
    }

    #[test]
    fn the_ca_debug_output_never_contains_the_key() {
        let ca = generate_managed_services_ca(now()).unwrap();
        let debug = format!("{ca:?}");
        assert!(!debug.contains("PRIVATE KEY"), "{debug}");
        assert!(debug.contains(&ca.fingerprint_sha256));
    }

    #[test]
    fn a_server_certificate_names_the_service_and_is_server_auth_only() {
        let ca = generate_managed_services_ca(now()).unwrap();
        let sans = vec![
            "postgres-orders".to_string(),
            "orders.temps.local".to_string(),
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ];
        let leaf =
            issue_server_certificate(&ca.cert_pem, &ca.key_pem, "postgres-orders", &sans, now())
                .unwrap();
        assert_eq!(leaf.subject_alt_names, sans);
        assert_eq!(
            leaf.not_after - leaf.not_before,
            Duration::days(SERVER_CERTIFICATE_VALIDITY_DAYS)
        );
        assert_ne!(leaf.fingerprint_sha256, ca.fingerprint_sha256);
        assert!(!format!("{leaf:?}").contains("PRIVATE KEY"));

        let der = der(&leaf.cert_pem);
        let (_, cert) = X509Certificate::from_der(&der).unwrap();
        assert!(!cert.is_ca());
        let mut dns = Vec::new();
        let mut ips = Vec::new();
        let mut eku_server = false;
        let mut eku_client = false;
        for ext in cert.extensions() {
            match ext.parsed_extension() {
                ParsedExtension::SubjectAlternativeName(san) => {
                    for name in &san.general_names {
                        match name {
                            GeneralName::DNSName(n) => dns.push(n.to_string()),
                            GeneralName::IPAddress(b) => ips.push(b.to_vec()),
                            _ => {}
                        }
                    }
                }
                ParsedExtension::ExtendedKeyUsage(eku) => {
                    eku_server = eku.server_auth;
                    eku_client = eku.client_auth;
                }
                _ => {}
            }
        }
        assert_eq!(
            dns,
            vec!["postgres-orders", "orders.temps.local", "localhost"]
        );
        assert_eq!(ips, vec![vec![127, 0, 0, 1]]);
        assert!(eku_server, "leaf must allow serverAuth");
        assert!(
            !eku_client,
            "a database key must never be a client identity"
        );
        let ku = cert.key_usage().unwrap().unwrap().value;
        assert!(ku.digital_signature());
        assert!(!ku.key_cert_sign(), "a leaf must not sign certificates");
        // Issued by the managed-services CA.
        let ca_der = super::tests::der(&ca.cert_pem);
        let (_, ca_cert) = X509Certificate::from_der(&ca_der).unwrap();
        assert_eq!(cert.issuer(), ca_cert.subject());
        cert.verify_signature(Some(ca_cert.public_key())).unwrap();
    }

    #[test]
    fn a_certificate_signed_by_another_ca_does_not_verify_under_this_one() {
        let ours = generate_managed_services_ca(now()).unwrap();
        let other = generate_managed_services_ca(now()).unwrap();
        let leaf = issue_server_certificate(
            &other.cert_pem,
            &other.key_pem,
            "postgres-x",
            &["postgres-x".to_string()],
            now(),
        )
        .unwrap();
        let leaf_der = der(&leaf.cert_pem);
        let (_, cert) = X509Certificate::from_der(&leaf_der).unwrap();
        let ours_der = der(&ours.cert_pem);
        let (_, ours_cert) = X509Certificate::from_der(&ours_der).unwrap();
        assert!(cert.verify_signature(Some(ours_cert.public_key())).is_err());
    }

    #[test]
    fn issuing_without_any_name_is_refused_with_the_subject() {
        let ca = generate_managed_services_ca(now()).unwrap();
        let err = issue_server_certificate(
            &ca.cert_pem,
            &ca.key_pem,
            "postgres-empty",
            &[" ".to_string(), String::new()],
            now(),
        )
        .unwrap_err();
        assert!(matches!(err, ManagedPkiError::NoSubjectAltNames { .. }));
        assert!(err.to_string().contains("postgres-empty"));
    }

    #[test]
    fn a_corrupt_ca_key_is_reported_as_an_invalid_authority() {
        let ca = generate_managed_services_ca(now()).unwrap();
        let err = issue_server_certificate(
            &ca.cert_pem,
            "not a key",
            "postgres-x",
            &["postgres-x".to_string()],
            now(),
        )
        .unwrap_err();
        assert!(
            matches!(err, ManagedPkiError::InvalidAuthority { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_non_ascii_name_is_rejected_with_the_offending_value() {
        let ca = generate_managed_services_ca(now()).unwrap();
        let err = issue_server_certificate(
            &ca.cert_pem,
            &ca.key_pem,
            "postgres-x",
            &["bücher.example".to_string()],
            now(),
        )
        .unwrap_err();
        assert!(
            matches!(err, ManagedPkiError::InvalidSubjectAltName { ref value, .. } if value == "bücher.example"),
            "{err}"
        );
    }

    #[test]
    fn subject_alt_names_are_trimmed_and_deduplicated_case_insensitively() {
        let names = normalize_subject_alt_names(&[
            " postgres-a ".to_string(),
            "POSTGRES-A".to_string(),
            "a.temps.local.".to_string(),
            String::new(),
            "127.0.0.1".to_string(),
            "127.0.0.1".to_string(),
        ]);
        assert_eq!(names, vec!["postgres-a", "a.temps.local", "127.0.0.1"]);
    }

    #[test]
    fn renewal_starts_thirty_days_before_expiry() {
        let now = now();
        assert!(!needs_renewal(now + Duration::days(31), now));
        assert!(needs_renewal(now + Duration::days(29), now));
        assert!(needs_renewal(now - Duration::days(1), now));
    }

    #[test]
    fn serials_are_positive_and_unique() {
        let a = random_serial();
        let b = random_serial();
        assert_ne!(a.as_ref(), b.as_ref());
        assert_eq!(a.as_ref().len(), 16);
        assert!(a.as_ref()[0] & 0x80 == 0);
    }
}
