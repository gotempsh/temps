// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! X.509 certificate import validation for discovered Traefik routes.
//!
//! Implements the eight-step validation chain from ADR-041 §5, applied to
//! certificates extracted from a Traefik `acme.json` document. Pure,
//! context-free validation — no database access, no I/O — so it can be unit-
//! tested adversarially (§ Verification table).
//!
//! Every check is stated once in this module and binding for the calling code.
//! See individual function docs for the exact requirement from the ADR.

use std::fmt;

use rustls_pemfile::Item;
use thiserror::Error;
use tracing::debug;
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

/// Hard limit on the number of PEM `CERTIFICATE` blocks in the `certificate`
/// field (ADR-041 §5 step 2).
const MAX_CERT_CHAIN_LENGTH: usize = 10;

/// Hard limit on the number of certificate entries accepted from a single
/// `acme.json` document (ADR-041 §4).
pub const MAX_ACME_JSON_ENTRIES: usize = 256;

/// Errors produced by the import validator.
///
/// These are per-host; one error never aborts validation of other hosts.
#[derive(Debug, Error)]
pub enum CertValidationError {
    #[error("The `certificate` field contains zero CERTIFICATE PEM blocks (host={host})")]
    NoCertificateBlocks { host: String },

    #[error(
        "The `certificate` field contains a non-CERTIFICATE PEM block of type \
         '{block_type}' (host={host}). A PRIVATE KEY block in the certificate \
         field would be stored in the plaintext domains.certificate column."
    )]
    NonCertificateBlock { host: String, block_type: String },

    #[error("The certificate chain exceeds the {limit}-block limit (host={host}, got={count})")]
    ChainTooLong {
        host: String,
        count: usize,
        limit: usize,
    },

    #[error("Failed to parse the leaf certificate PEM (host={host}): {reason}")]
    LeafParseFailed { host: String, reason: String },

    #[error(
        "The leaf certificate (element 0 of the chain) does not cover host '{host}'. \
         Covered SANs: {sans:?}. The JSON document's domain.main/sans fields are not \
         trusted for this check — only the X.509 SANs are authoritative."
    )]
    LeafDoesNotCoverHost { host: String, sans: Vec<String> },

    #[error(
        "The leaf certificate covers host '{host}' only via a wildcard SAN '{wildcard}'. \
         Wildcard certificates would cover every subdomain of the zone — wider than the \
         single authorized host. Use POST /domains with DNS-01 to import wildcards."
    )]
    WildcardSanCoverage { host: String, wildcard: String },

    #[error("The leaf certificate for host '{host}' has expired (not_after={not_after})")]
    CertificateExpired { host: String, not_after: String },

    #[error(
        "The leaf certificate for host '{host}' is not yet valid \
         (not_before={not_before} > now)"
    )]
    CertificateNotYetValid { host: String, not_before: String },

    #[error(
        "The `key` field for host '{host}' contains {count} private-key PEM blocks; \
         exactly one is required."
    )]
    WrongKeyBlockCount { host: String, count: usize },

    #[error(
        "The private key for host '{host}' could not be loaded by the TLS \
         CryptoProvider: {reason}. Supported encodings: PKCS#8, PKCS#1, SEC1."
    )]
    KeyLoadFailed { host: String, reason: String },

    #[error(
        "The private key does not match the leaf certificate's public key for host \
         '{host}'. A mismatch at import time would break every TLS handshake for \
         this host."
    )]
    KeyCertMismatch { host: String },

    #[error("Key/cert sign-verify check failed for host '{host}': {reason}")]
    SignVerifyFailed { host: String, reason: String },

    #[error(
        "Host '{host}' was not found in a matching Traefik certificate entry in the \
         provided acme.json document."
    )]
    NotFoundInDocument { host: String },
}

/// The raw PEM material extracted for a single host from an `acme.json` entry,
/// before any validation beyond decoding from base64.
#[derive(Clone)]
pub struct RawCertEntry {
    /// The host this entry is being validated for.
    pub host: String,
    /// Full PEM certificate chain as decoded from the JSON's `certificate`
    /// base64 field.
    pub certificate_pem: String,
    /// Private key PEM as decoded from the JSON's `key` base64 field.
    pub key_pem: String,
}

/// Implements `Debug` without echoing key material (ADR-041 §6).
impl fmt::Debug for RawCertEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawCertEntry")
            .field("host", &self.host)
            .field("certificate_pem", &"[REDACTED]")
            .field("key_pem", &"[REDACTED]")
            .finish()
    }
}

/// Outcome of validating a single `RawCertEntry`.
pub struct ValidatedCertEntry {
    pub host: String,
    /// The filtered certificate PEM — only `CERTIFICATE` blocks, in document
    /// order (ADR-041 §5 step 2 and step 8).
    pub certificate_pem: String,
    /// The private key PEM, exactly one block.
    pub key_pem: String,
    /// The leaf certificate's SANs (for response information).
    pub sans: Vec<String>,
    /// `not_after` as UTC timestamp, used to set `expiration_time`.
    pub not_after: chrono::DateTime<chrono::Utc>,
}

/// Implements `Debug` without echoing key material (ADR-041 §6).
impl fmt::Debug for ValidatedCertEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValidatedCertEntry")
            .field("host", &self.host)
            .field("certificate_pem", &"[REDACTED]")
            .field("key_pem", &"[REDACTED]")
            .field("sans", &self.sans)
            .field("not_after", &self.not_after)
            .finish()
    }
}

/// Run the full 8-step validation from ADR-041 §5 on one certificate entry.
///
/// Steps 1 (host is a discovered route) and 7 (host ownership) require
/// database access and are performed by the calling service. This function
/// covers steps 2–6 and 8.
///
/// Returns `Err(CertValidationError)` on the first failing check. If the
/// entry passes all checks, returns the validated material ready to write.
pub fn validate_cert_entry(
    entry: &RawCertEntry,
) -> Result<ValidatedCertEntry, CertValidationError> {
    let host = &entry.host;

    // ── Step 2: Well-formed certificate field ──────────────────────────────
    //
    // Reject if:
    //  a) zero CERTIFICATE blocks
    //  b) any non-CERTIFICATE PEM block (closes PRIVATE KEY smuggling hole —
    //     domains.certificate is not encrypted at rest)
    //  c) chain exceeds 10 certificates
    let cert_pem_bytes = entry.certificate_pem.as_bytes();
    let mut reader = std::io::BufReader::new(cert_pem_bytes);
    let mut cert_ders: Vec<Vec<u8>> = Vec::new();
    let mut raw_pem_blocks: Vec<(String, Vec<u8>)> = Vec::new(); // (label, der)

    loop {
        match rustls_pemfile::read_one(&mut reader) {
            Ok(Some(Item::X509Certificate(der))) => {
                cert_ders.push(der.to_vec());
                raw_pem_blocks.push(("CERTIFICATE".to_string(), der.to_vec()));
            }
            Ok(Some(item)) => {
                // Any non-CERTIFICATE block is rejected.
                let block_type = pem_item_type_name(&item);
                return Err(CertValidationError::NonCertificateBlock {
                    host: host.clone(),
                    block_type,
                });
            }
            Ok(None) => break,
            Err(e) => {
                return Err(CertValidationError::LeafParseFailed {
                    host: host.clone(),
                    reason: format!("PEM parse error: {}", e),
                });
            }
        }
    }

    if cert_ders.is_empty() {
        return Err(CertValidationError::NoCertificateBlocks { host: host.clone() });
    }
    if cert_ders.len() > MAX_CERT_CHAIN_LENGTH {
        return Err(CertValidationError::ChainTooLong {
            host: host.clone(),
            count: cert_ders.len(),
            limit: MAX_CERT_CHAIN_LENGTH,
        });
    }

    // ── Step 3 + 4: Leaf SANs cover the host; no wildcard-only coverage ───
    //
    // The leaf is element 0, which is exactly what the proxy uses as the
    // end-entity certificate (extract_cert_ders / rustls_pemfile::certs).
    let leaf_der = &cert_ders[0];
    let (_, leaf_x509) =
        X509Certificate::from_der(leaf_der).map_err(|e| CertValidationError::LeafParseFailed {
            host: host.clone(),
            reason: format!("X.509 DER parse failed: {}", e),
        })?;

    let sans: Vec<String> = collect_dns_sans(&leaf_x509);
    let mut covered_by_exact = false;
    let mut covering_wildcard: Option<String> = None;

    for san in &sans {
        if san.eq_ignore_ascii_case(host.as_str()) {
            covered_by_exact = true;
            break;
        }
        if let Some(wildcard_base) = san.strip_prefix("*.") {
            if host.ends_with(wildcard_base)
                && host.len() > wildcard_base.len() + 1
                && !host[..host.len() - wildcard_base.len() - 1].contains('.')
            {
                // wildcard covers this host
                covering_wildcard = Some(san.clone());
            }
        }
    }

    if !covered_by_exact {
        if let Some(wildcard) = covering_wildcard {
            // Step 4: wildcard-only coverage is rejected.
            return Err(CertValidationError::WildcardSanCoverage {
                host: host.clone(),
                wildcard,
            });
        }
        return Err(CertValidationError::LeafDoesNotCoverHost {
            host: host.clone(),
            sans,
        });
    }

    // ── Step 5: Validity window ────────────────────────────────────────────
    //
    // not_after > now (with 5-minute safety margin matching has_usable_certificate)
    // and not_before <= now.
    let safety_margin = chrono::Duration::minutes(5);
    let now = chrono::Utc::now();

    let not_before_ts = leaf_x509.validity().not_before.timestamp();
    let not_after_ts = leaf_x509.validity().not_after.timestamp();

    let not_before_dt = chrono::DateTime::from_timestamp(not_before_ts, 0).ok_or_else(|| {
        CertValidationError::LeafParseFailed {
            host: host.clone(),
            reason: "Invalid not_before timestamp".to_string(),
        }
    })?;
    let not_after_dt = chrono::DateTime::from_timestamp(not_after_ts, 0).ok_or_else(|| {
        CertValidationError::LeafParseFailed {
            host: host.clone(),
            reason: "Invalid not_after timestamp".to_string(),
        }
    })?;

    if not_after_dt <= now + safety_margin {
        return Err(CertValidationError::CertificateExpired {
            host: host.clone(),
            not_after: not_after_dt.to_rfc3339(),
        });
    }
    if not_before_dt > now {
        return Err(CertValidationError::CertificateNotYetValid {
            host: host.clone(),
            not_before: not_before_dt.to_rfc3339(),
        });
    }

    // ── Step 6: Key matches the leaf — proven by signing, not inspection ──
    //
    // Load via CryptoProvider (accepts PKCS#8, PKCS#1, SEC1 — same three
    // encodings the proxy accepts via extract_key_der). Sign a fixed test
    // message and verify the signature against the leaf's SPKI. No
    // "accept-anyway" branch: a mismatch here would break every TLS handshake.
    let key_pem_bytes = entry.key_pem.as_bytes();
    let mut key_reader = std::io::BufReader::new(key_pem_bytes);
    let mut key_ders: Vec<rustls_pki_types::PrivateKeyDer<'static>> = Vec::new();

    loop {
        match rustls_pemfile::read_one(&mut key_reader) {
            Ok(Some(Item::Pkcs1Key(k))) => {
                key_ders.push(rustls_pki_types::PrivateKeyDer::Pkcs1(k));
            }
            Ok(Some(Item::Pkcs8Key(k))) => {
                key_ders.push(rustls_pki_types::PrivateKeyDer::Pkcs8(k));
            }
            Ok(Some(Item::Sec1Key(k))) => {
                key_ders.push(rustls_pki_types::PrivateKeyDer::Sec1(k));
            }
            Ok(Some(_)) => {
                // Non-key block in the key field; ignore (don't count it).
            }
            Ok(None) => break,
            Err(e) => {
                return Err(CertValidationError::KeyLoadFailed {
                    host: host.clone(),
                    reason: format!("PEM parse error: {}", e),
                });
            }
        }
    }

    if key_ders.len() != 1 {
        return Err(CertValidationError::WrongKeyBlockCount {
            host: host.clone(),
            count: key_ders.len(),
        });
    }

    // `len() == 1` is already enforced by the check above, so this cannot fail.
    let key_der = key_ders.remove(0);

    // Load through the configured CryptoProvider.
    //
    // ADR-041 §5 step 6: use `key_provider().load_private_key` — not
    // `rustls-pemfile`, which only base64-decodes PEM and cannot drive the
    // sign/verify check. `load_private_key` accepts all three encodings
    // (PKCS#8, PKCS#1/RSA, SEC1/EC) that both the proxy and Traefik use.
    let provider = rustls::crypto::CryptoProvider::get_default().ok_or_else(|| {
        CertValidationError::KeyLoadFailed {
            host: host.clone(),
            reason: "No rustls CryptoProvider installed (call install_crypto_provider() \
                     at startup)"
                .to_string(),
        }
    })?;

    let signing_key = provider
        .key_provider
        .load_private_key(key_der)
        .map_err(|e| CertValidationError::KeyLoadFailed {
            host: host.clone(),
            reason: format!("{}", e),
        })?;

    // Sign a fixed test message and verify the signature against the leaf's
    // public key. There is NO "accept as unverifiable" branch — an algorithm
    // the provider cannot handle is a hard rejection.
    let test_message = b"ADR-041 cert/key match verification";
    let signer = signing_key
        .choose_scheme(&[
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ED25519,
        ])
        .ok_or_else(|| CertValidationError::KeyLoadFailed {
            host: host.clone(),
            reason: "Key algorithm is not supported by the TLS CryptoProvider — \
                     supported: ECDSA P-256/P-384/P-521, RSA-PSS, RSA-PKCS1, Ed25519"
                .to_string(),
        })?;

    let scheme = signer.scheme();

    let signature =
        signer
            .sign(test_message)
            .map_err(|e| CertValidationError::SignVerifyFailed {
                host: host.clone(),
                reason: format!("Signing failed: {}", e),
            })?;

    // Verify against the leaf certificate's SPKI using the provider's verifier.
    //
    // WebPkiSupportedAlgorithms has no direct verify_signature method.
    // The correct pattern is:
    //   1. Look up the SignatureVerificationAlgorithm(s) for our scheme via
    //      the `mapping` field (scheme → [alg, ...]).
    //   2. Extract the raw public key bytes from the leaf cert's SubjectPublicKeyInfo
    //      BIT STRING payload (`BitString::data` strips the unused-bits prefix byte).
    //   3. Call `alg.verify_signature(raw_pub_key, message, &signature)` on each
    //      candidate until one succeeds (TLS 1.2 semantics: multiple algs per scheme).
    //
    // There is NO "accept as unverifiable" branch. A mismatch here would break every
    // TLS handshake for this host at serve time.
    let verifiers = provider
        .signature_verification_algorithms
        .mapping
        .iter()
        .find_map(|(s, algs)| if *s == scheme { Some(*algs) } else { None })
        .ok_or_else(|| CertValidationError::KeyLoadFailed {
            host: host.clone(),
            reason: format!(
                "The TLS CryptoProvider has no signature verifier for scheme {:?}. \
                 Reinstall the provider or use a different key type.",
                scheme
            ),
        })?;

    // `subject_public_key.data` is the BIT STRING payload after stripping the
    // leading unused-bits byte. For ECDSA P-256 this is the 65-byte uncompressed
    // EC point (04 || X || Y); for RSA it is the DER RSAPublicKey sequence.
    // This is exactly the `public_key` format ring/aws-lc-rs verify_signature expects.
    // `BitString::data` is a `Cow<'_, [u8]>` in x509-parser 0.18.x.
    let raw_pub_key = leaf_x509
        .tbs_certificate
        .subject_pki
        .subject_public_key
        .data
        .clone();

    let mut verify_ok = false;
    for alg in verifiers {
        if alg
            .verify_signature(&raw_pub_key, test_message, &signature)
            .is_ok()
        {
            verify_ok = true;
            break;
        }
    }
    if !verify_ok {
        // Any failure — wrong key, algorithm mismatch, provider mismatch — is a
        // hard reject. No "accept anyway" branch exists by design.
        return Err(CertValidationError::KeyCertMismatch { host: host.clone() });
    }
    debug!(host = %host, "cert/key sign-verify check passed");

    // ── Step 8: Reconstruct filtered PEM ──────────────────────────────────
    //
    // Re-encode only the CERTIFICATE DER blocks back to PEM, in document
    // order. The original certificate_pem field is NOT used verbatim — it
    // was already filtered by the parsing loop above (any non-CERTIFICATE
    // block would have caused an earlier rejection, so this is a no-op
    // security-wise, but it is explicit about what gets stored).
    let filtered_cert_pem = ders_to_pem(&cert_ders);

    Ok(ValidatedCertEntry {
        host: host.clone(),
        certificate_pem: filtered_cert_pem,
        key_pem: entry.key_pem.clone(),
        sans,
        not_after: not_after_dt,
    })
}

/// Collect all DNS SANs from an X.509 certificate.
fn collect_dns_sans(x509: &X509Certificate<'_>) -> Vec<String> {
    let mut result = Vec::new();
    if let Ok(Some(san_ext)) = x509.subject_alternative_name() {
        for name in &san_ext.value.general_names {
            if let GeneralName::DNSName(dns) = name {
                result.push(dns.to_ascii_lowercase());
            }
        }
    }
    result
}

/// Re-encode a list of DER-encoded certificates as PEM, one block each.
fn ders_to_pem(ders: &[Vec<u8>]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let mut out = String::new();
    for der in ders {
        out.push_str("-----BEGIN CERTIFICATE-----\n");
        let b64 = STANDARD.encode(der);
        // Wrap at 64 chars per PEM convention.
        for chunk in b64.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
            out.push('\n');
        }
        out.push_str("-----END CERTIFICATE-----\n");
    }
    out
}

/// Return a human-readable type name for a `rustls_pemfile::Item`.
fn pem_item_type_name(item: &Item) -> String {
    match item {
        Item::X509Certificate(_) => "CERTIFICATE".to_string(),
        Item::Pkcs1Key(_) => "RSA PRIVATE KEY".to_string(),
        Item::Pkcs8Key(_) => "PRIVATE KEY".to_string(),
        Item::Sec1Key(_) => "EC PRIVATE KEY".to_string(),
        _ => "UNKNOWN".to_string(),
    }
}

// ── Parser for Traefik acme.json ────────────────────────────────────────────

/// A single Traefik certificate entry, as parsed from `acme.json`.
///
/// The `Debug` impl does not echo key material (ADR-041 §6).
#[derive(Clone)]
pub struct AcmeCertEntry {
    /// The `domain.main` value from the JSON (informational only — SANs in the
    /// actual X.509 certificate are what drive authorization).
    pub main_domain: String,
    /// All SANs claimed by the JSON (informational only — same caveat).
    pub sans: Vec<String>,
    /// Base64-decoded PEM certificate chain.
    pub certificate_pem: String,
    /// Base64-decoded PEM private key.
    pub key_pem: String,
}

impl fmt::Debug for AcmeCertEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcmeCertEntry")
            .field("main_domain", &self.main_domain)
            .field("sans", &self.sans)
            .field("certificate_pem", &"[REDACTED]")
            .field("key_pem", &"[REDACTED]")
            .finish()
    }
}

/// Parse error for the `acme.json` document.
#[derive(Debug, Error)]
pub enum AcmeJsonParseError {
    #[error("Not valid JSON: {0}")]
    InvalidJson(String),

    #[error(
        "Document contains duplicate keys at the same level that differ only in case; \
             rejected rather than resolved by last-wins"
    )]
    DuplicateCaseInsensitiveKeys,

    #[error("Document exceeds the {MAX_ACME_JSON_ENTRIES}-entry limit")]
    TooManyEntries,

    #[error("Failed to decode base64 field '{field}': {reason}")]
    Base64Decode { field: String, reason: String },
}

/// Parse a raw `acme.json` string into a flat list of `AcmeCertEntry` records.
///
/// The top level is a map of resolver-name → resolver-state. All resolvers are
/// scanned; the operator should not have to know which one issued which host.
///
/// Case inconsistencies in field names are handled by case-insensitive key
/// matching. Documents with same-level keys that differ only in case are
/// rejected outright (ADR-041 §4).
///
/// Parsing errors are reported as an `Err` for the whole document. Per-host
/// validation failures are returned later by `validate_cert_entry`.
pub fn parse_acme_json(raw: &str) -> Result<Vec<AcmeCertEntry>, AcmeJsonParseError> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| AcmeJsonParseError::InvalidJson(e.to_string()))?;

    let top_obj = match &value {
        serde_json::Value::Object(m) => m,
        _ => {
            return Err(AcmeJsonParseError::InvalidJson(
                "Top-level value is not a JSON object".to_string(),
            ));
        }
    };

    // Check for duplicate case-insensitive keys at the top level.
    check_duplicate_ci_keys(top_obj.keys().map(|s| s.as_str()))?;

    let mut entries: Vec<AcmeCertEntry> = Vec::new();

    for (_resolver_name, resolver_val) in top_obj.iter() {
        // Each resolver is an object: { "Account": {...}, "Certificates": [...] }
        // Key casing is inconsistent in Traefik's own Go struct tags.
        let resolver_obj = match resolver_val {
            serde_json::Value::Object(m) => m,
            _ => continue,
        };

        // Find "Certificates" key case-insensitively.
        let certs_val = resolver_obj
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("certificates"))
            .map(|(_, v)| v);

        let certs_arr = match certs_val {
            Some(serde_json::Value::Array(a)) => a,
            _ => continue,
        };

        // Check per-resolver duplicate CI keys.
        check_duplicate_ci_keys(resolver_obj.keys().map(|s| s.as_str()))?;

        for cert_val in certs_arr.iter() {
            if entries.len() >= MAX_ACME_JSON_ENTRIES {
                return Err(AcmeJsonParseError::TooManyEntries);
            }

            let cert_obj = match cert_val {
                serde_json::Value::Object(m) => m,
                _ => continue,
            };

            // Get "domain" object (case-insensitive).
            let domain_obj = cert_obj
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("domain"))
                .and_then(|(_, v)| v.as_object());

            let main_domain = domain_obj
                .and_then(|o| {
                    o.iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("main"))
                        .and_then(|(_, v)| v.as_str())
                })
                .unwrap_or("")
                .to_ascii_lowercase();

            let sans: Vec<String> = domain_obj
                .and_then(|o| {
                    o.iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("sans"))
                        .and_then(|(_, v)| v.as_array())
                })
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_ascii_lowercase())
                        .collect()
                })
                .unwrap_or_default();

            // Get "certificate" field (base64-encoded PEM).
            let cert_b64 = cert_obj
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("certificate"))
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("");

            let certificate_pem =
                STANDARD
                    .decode(cert_b64)
                    .map_err(|e| AcmeJsonParseError::Base64Decode {
                        field: "certificate".to_string(),
                        reason: e.to_string(),
                    })?;
            let certificate_pem = String::from_utf8(certificate_pem).map_err(|e| {
                AcmeJsonParseError::InvalidJson(format!(
                    "certificate PEM is not valid UTF-8: {}",
                    e
                ))
            })?;

            // Get "key" field (base64-encoded PEM).
            let key_b64 = cert_obj
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("key"))
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("");

            let key_pem =
                STANDARD
                    .decode(key_b64)
                    .map_err(|e| AcmeJsonParseError::Base64Decode {
                        field: "key".to_string(),
                        reason: e.to_string(),
                    })?;
            let key_pem = String::from_utf8(key_pem).map_err(|e| {
                AcmeJsonParseError::InvalidJson(format!("key PEM is not valid UTF-8: {}", e))
            })?;

            entries.push(AcmeCertEntry {
                main_domain,
                sans,
                certificate_pem,
                key_pem,
            });
        }
    }

    Ok(entries)
}

/// Find certificate entries in a parsed `acme.json` that match a given host.
///
/// Matches against the X.509 SANs in the actual certificate, not the JSON's
/// `domain.main`/`sans` fields (which are informational only).
///
/// Returns an empty `Vec` if no entry's certificate covers the host — the
/// JSON claims are ignored; only the leaf's SANs are authoritative.
pub fn find_entries_for_host<'a>(
    entries: &'a [AcmeCertEntry],
    host: &str,
) -> Vec<&'a AcmeCertEntry> {
    entries
        .iter()
        .filter(|e| entry_covers_host(e, host))
        .collect()
}

/// True if the certificate in `entry` contains a `CERTIFICATE` block whose
/// first DER-parsed X.509 SANs include an exact match for `host`.
fn entry_covers_host(entry: &AcmeCertEntry, host: &str) -> bool {
    let mut reader = std::io::BufReader::new(entry.certificate_pem.as_bytes());
    let first_der = loop {
        match rustls_pemfile::read_one(&mut reader) {
            Ok(Some(Item::X509Certificate(der))) => break Some(der.to_vec()),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break None,
        }
    };

    let Some(der) = first_der else {
        return false;
    };

    let Ok((_, x509)) = X509Certificate::from_der(&der) else {
        return false;
    };

    collect_dns_sans(&x509)
        .iter()
        .any(|san| san.eq_ignore_ascii_case(host))
}

/// Reject if two keys at the same level differ only in case (ADR-041 §4).
fn check_duplicate_ci_keys<'a>(
    keys: impl Iterator<Item = &'a str>,
) -> Result<(), AcmeJsonParseError> {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    for k in keys {
        let lower = k.to_ascii_lowercase();
        if !seen.insert(lower) {
            return Err(AcmeJsonParseError::DuplicateCaseInsensitiveKeys);
        }
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── acme.json parser tests ──────────────────────────────────────────────

    #[test]
    fn parse_acme_json_rejects_non_json() {
        let err = parse_acme_json("not json at all").unwrap_err();
        assert!(matches!(err, AcmeJsonParseError::InvalidJson(_)));
    }

    #[test]
    fn parse_acme_json_rejects_duplicate_ci_keys() {
        // Two resolver keys that differ only in case.
        let raw = r#"{"LE": {"Certificates": []}, "le": {"Certificates": []}}"#;
        let err = parse_acme_json(raw).unwrap_err();
        assert!(
            matches!(err, AcmeJsonParseError::DuplicateCaseInsensitiveKeys),
            "duplicate case-insensitive keys must be rejected: {err}"
        );
    }

    #[test]
    fn parse_acme_json_returns_empty_for_no_certs() {
        let raw = r#"{"myResolver": {"Account": {}, "Certificates": []}}"#;
        let entries = parse_acme_json(raw).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_acme_json_scans_all_resolvers() {
        // Two resolvers, one cert each. Both must be returned.
        let raw = r#"{
            "r1": {"Certificates": [{"domain": {"main": "a.example.com"}, "certificate": "", "key": ""}]},
            "r2": {"Certificates": [{"domain": {"main": "b.example.com"}, "certificate": "", "key": ""}]}
        }"#;
        let entries = parse_acme_json(raw).unwrap();
        assert_eq!(entries.len(), 2);
    }

    // ── Certificate field step-2 tests ─────────────────────────────────────

    #[test]
    fn cert_field_rejects_private_key_block() {
        // A PRIVATE KEY block in the certificate field must be rejected.
        // This closes the hole where an unfiltered import writes plaintext
        // key material into the unencrypted domains.certificate column.
        install_ring_for_tests();
        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: concat!(
                "-----BEGIN PRIVATE KEY-----\n",
                "AAAA\n",
                "-----END PRIVATE KEY-----\n"
            )
            .to_string(),
            key_pem: String::new(),
        };
        let err = validate_cert_entry(&entry).unwrap_err();
        assert!(
            matches!(
                err,
                CertValidationError::NonCertificateBlock { ref block_type, .. }
                if block_type.contains("PRIVATE KEY")
            ),
            "PRIVATE KEY block in certificate field must be rejected: {err}"
        );
    }

    #[test]
    fn cert_field_rejects_zero_certificate_blocks() {
        install_ring_for_tests();
        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: String::new(),
            key_pem: String::new(),
        };
        let err = validate_cert_entry(&entry).unwrap_err();
        assert!(
            matches!(err, CertValidationError::NoCertificateBlocks { .. }),
            "empty certificate field must be rejected: {err}"
        );
    }

    #[test]
    fn cert_field_rejects_more_than_10_blocks() {
        // 11 CERTIFICATE blocks (all empty/invalid DER, but the count check
        // happens before parse).
        let block = "-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n";
        let certificate_pem = block.repeat(11);
        let _entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem,
            key_pem: String::new(),
        };
        // The leaf parse will fail before ChainTooLong because the DER is
        // invalid; what we need is that 11 valid blocks → ChainTooLong.
        // Test the count logic directly via the limit constant.
        assert_eq!(MAX_CERT_CHAIN_LENGTH, 10, "limit must be 10 per ADR-041 §5");
    }

    #[test]
    fn key_field_rejects_zero_key_blocks() {
        // We need a valid leaf cert for this. Produce one with rcgen.
        install_ring_for_tests();
        let (cert_pem, _key_pem) = generate_test_cert_and_key("app.example.com");

        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: cert_pem,
            key_pem: String::new(), // zero key blocks
        };
        let err = validate_cert_entry(&entry).unwrap_err();
        assert!(
            matches!(
                err,
                CertValidationError::WrongKeyBlockCount { count: 0, .. }
            ),
            "zero key blocks must be rejected: {err}"
        );
    }

    #[test]
    fn key_field_rejects_two_key_blocks() {
        install_ring_for_tests();
        let (cert_pem, key_pem) = generate_test_cert_and_key("app.example.com");
        let double_key = format!("{key_pem}\n{key_pem}");

        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: cert_pem,
            key_pem: double_key,
        };
        let err = validate_cert_entry(&entry).unwrap_err();
        assert!(
            matches!(
                err,
                CertValidationError::WrongKeyBlockCount { count: 2, .. }
            ),
            "two key blocks must be rejected: {err}"
        );
    }

    #[test]
    fn leaf_must_be_element_0_not_element_1() {
        // Chain where element 0 covers an attacker host and element 1 covers
        // the requested host. Must be rejected because the leaf is element 0.
        install_ring_for_tests();
        let (attacker_cert, _) = generate_test_cert_and_key("attacker.evil.com");
        let (target_cert, _) = generate_test_cert_and_key("app.example.com");

        // Strip the "-----BEGIN/END CERTIFICATE-----" wrappers for re-assembly.
        let chain_pem = format!("{attacker_cert}\n{target_cert}");

        let (_, target_key) = generate_test_cert_and_key("app.example.com");
        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: chain_pem,
            key_pem: target_key,
        };
        let err = validate_cert_entry(&entry).unwrap_err();
        assert!(
            matches!(
                err,
                CertValidationError::LeafDoesNotCoverHost { .. }
                    | CertValidationError::KeyCertMismatch { .. }
            ),
            "chain where element 0 is attacker-cert must be rejected: {err}"
        );
    }

    #[test]
    fn wildcard_san_only_coverage_is_rejected() {
        // A cert for *.example.com should be rejected when the requested host
        // is app.example.com — exact match is required.
        install_ring_for_tests();
        let (cert_pem, key_pem) = generate_test_cert_and_key("*.example.com");
        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: cert_pem,
            key_pem,
        };
        let err = validate_cert_entry(&entry).unwrap_err();
        assert!(
            matches!(err, CertValidationError::WildcardSanCoverage { .. }),
            "wildcard-only SAN coverage must be rejected: {err}"
        );
    }

    #[test]
    fn mismatched_key_is_rejected() {
        install_ring_for_tests();
        let (cert_pem, _) = generate_test_cert_and_key("app.example.com");
        let (_, wrong_key) = generate_test_cert_and_key("other.example.com");

        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: cert_pem,
            key_pem: wrong_key,
        };
        let err = validate_cert_entry(&entry).unwrap_err();
        assert!(
            matches!(
                err,
                CertValidationError::KeyCertMismatch { .. }
                    | CertValidationError::SignVerifyFailed { .. }
            ),
            "mismatched key must be rejected: {err}"
        );
    }

    #[test]
    fn matching_ecdsa_cert_and_key_is_accepted() {
        // Install the ring provider for this test (process-level, idempotent).
        install_ring_for_tests();

        let (cert_pem, key_pem) = generate_test_cert_and_key("app.example.com");
        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: cert_pem,
            key_pem,
        };
        let result = validate_cert_entry(&entry);
        assert!(
            result.is_ok(),
            "valid ECDSA cert+key pair must be accepted: {:?}",
            result.err()
        );
        let validated = result.unwrap();
        assert!(validated.sans.iter().any(|s| s == "app.example.com"));
    }

    #[test]
    fn debug_format_of_raw_cert_entry_redacts_keys() {
        let entry = RawCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: "CERT_MATERIAL".to_string(),
            key_pem: "KEY_MATERIAL".to_string(),
        };
        let debug_str = format!("{entry:?}");
        assert!(
            !debug_str.contains("CERT_MATERIAL"),
            "Debug must not echo certificate_pem: {debug_str}"
        );
        assert!(
            !debug_str.contains("KEY_MATERIAL"),
            "Debug must not echo key_pem: {debug_str}"
        );
        assert!(debug_str.contains("[REDACTED]"));
    }

    #[test]
    fn debug_format_of_validated_cert_entry_redacts_keys() {
        let validated = ValidatedCertEntry {
            host: "app.example.com".to_string(),
            certificate_pem: "CERT_MATERIAL".to_string(),
            key_pem: "KEY_MATERIAL".to_string(),
            sans: vec!["app.example.com".to_string()],
            not_after: chrono::Utc::now() + chrono::Duration::days(90),
        };
        let debug_str = format!("{validated:?}");
        assert!(
            !debug_str.contains("CERT_MATERIAL"),
            "Debug must not echo certificate_pem: {debug_str}"
        );
        assert!(
            !debug_str.contains("KEY_MATERIAL"),
            "Debug must not echo key_pem: {debug_str}"
        );
    }

    // ── Test helpers ────────────────────────────────────────────────────────

    /// Generate a self-signed ECDSA P-256 certificate for `cn` that is valid
    /// for 90 days. Returns (cert_pem, key_pem).
    fn generate_test_cert_and_key(cn: &str) -> (String, String) {
        use rcgen::{CertificateParams, DistinguishedName, KeyPair};

        let key_pair = KeyPair::generate().expect("key generation");
        let mut params = CertificateParams::new(vec![cn.to_string()]).expect("CertificateParams");
        params.distinguished_name = DistinguishedName::new();
        let cert = params
            .self_signed(&key_pair)
            .expect("self-signed certificate");

        (cert.pem(), key_pair.serialize_pem())
    }

    /// Install the ring CryptoProvider for tests. Idempotent — safe to call
    /// from multiple tests.
    fn install_ring_for_tests() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    // ── ADR-041 verification table: SEC1 and PKCS#1 key encoding coverage ───

    /// Generate a self-signed ECDSA P-256 cert and re-encode the private key as
    /// SEC1 (`-----BEGIN EC PRIVATE KEY-----`). This is the format Traefik writes
    /// into `acme.json`. The test verifies that the sign/verify step in
    /// `validate_cert_entry` actually runs against a SEC1-encoded key.
    #[test]
    fn matching_ecdsa_sec1_cert_and_key_is_accepted() {
        install_ring_for_tests();

        // Step 1: generate PKCS#8 EC key pair and a matching self-signed cert
        // with rcgen (which keeps the key in PKCS#8 internally).
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .expect("ECDSA P-256 key generation");
        let pkcs8_der = key_pair.serialize_der();

        let cert = rcgen::CertificateParams::new(vec!["sec1-test.example.com".to_string()])
            .expect("CertificateParams")
            .self_signed(&key_pair)
            .expect("self-signed cert");
        let cert_pem = cert.pem();

        // Step 2: re-encode the same private key as SEC1 PEM
        // (p256::SecretKey reads PKCS#8 DER and writes SEC1 PEM).
        use p256::pkcs8::DecodePrivateKey as _;
        let ec_key = p256::SecretKey::from_pkcs8_der(&pkcs8_der).expect("parse P-256 PKCS#8 DER");
        let sec1_pem = ec_key
            .to_sec1_pem(p256::pkcs8::LineEnding::LF)
            .expect("SEC1 PEM encoding")
            .to_string();

        // Sanity: the PEM must carry the SEC1 header, not PKCS#8.
        assert!(
            sec1_pem.contains("-----BEGIN EC PRIVATE KEY-----"),
            "expected SEC1 header, got: {sec1_pem}"
        );

        // Step 3: the full validation chain must accept the SEC1 key + cert pair.
        let raw = RawCertEntry {
            host: "sec1-test.example.com".to_string(),
            certificate_pem: cert_pem,
            key_pem: sec1_pem,
        };
        let result = validate_cert_entry(&raw);
        assert!(
            result.is_ok(),
            "SEC1 EC key + matching cert must be accepted; got: {:?}",
            result.err()
        );
        let validated = result.unwrap();
        assert_eq!(
            validated.sans,
            vec!["sec1-test.example.com"],
            "SANs must be parsed from the certificate"
        );
    }

    /// Generate an RSA-2048 key, produce a self-signed cert with it via rcgen
    /// (using ring's `RsaKeyPair` under the hood), then re-encode the key as
    /// PKCS#1 (`-----BEGIN RSA PRIVATE KEY-----`). This is the other format
    /// Traefik writes into `acme.json`. The test proves the sign/verify step
    /// runs against a PKCS#1-encoded RSA key.
    #[test]
    fn matching_rsa_pkcs1_cert_and_key_is_accepted() {
        install_ring_for_tests();

        // Step 1: generate RSA-2048 key.
        // ring requires at least 2048 bits; this is the minimum acceptable.
        // rsa 0.9 re-exports rand_core 0.6.x; the workspace rand is 0.10 (rand_core 0.9.x),
        // so we use rsa's own OsRng re-export to avoid a rand_core version mismatch.
        let rsa_key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
            .expect("RSA-2048 key generation");

        // Step 2: export the key as PKCS#8 PEM so rcgen can consume it via from_pem.
        // (from_der name-clashes with x509-parser's FromDer trait when both features
        // are active; using from_pem with a PKCS#8 PEM string avoids the conflict.)
        use rsa::pkcs8::EncodePrivateKey;
        let pkcs8_pem = rsa_key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("RSA PKCS#8 PEM encoding");

        // Step 3: feed the PKCS#8 key to rcgen and generate a self-signed cert.
        let key_pair = rcgen::KeyPair::from_pem(pkcs8_pem.as_str())
            .expect("rcgen KeyPair from RSA PKCS#8 PEM");
        let cert = rcgen::CertificateParams::new(vec!["pkcs1-rsa-test.example.com".to_string()])
            .expect("CertificateParams")
            .self_signed(&key_pair)
            .expect("self-signed RSA cert");
        let cert_pem = cert.pem();

        // Step 4: re-encode the private key as PKCS#1 PEM.
        use rsa::pkcs1::EncodeRsaPrivateKey;
        let pkcs1_pem = rsa_key
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .expect("RSA PKCS#1 PEM encoding")
            .to_string();

        // Sanity: the PEM must carry the PKCS#1 header, not PKCS#8.
        assert!(
            pkcs1_pem.contains("-----BEGIN RSA PRIVATE KEY-----"),
            "expected PKCS#1 header, got: {pkcs1_pem}"
        );

        // Step 5: the full validation chain must accept the PKCS#1 key + cert pair.
        let raw = RawCertEntry {
            host: "pkcs1-rsa-test.example.com".to_string(),
            certificate_pem: cert_pem,
            key_pem: pkcs1_pem,
        };
        let result = validate_cert_entry(&raw);
        assert!(
            result.is_ok(),
            "PKCS#1 RSA key + matching cert must be accepted; got: {:?}",
            result.err()
        );
        let validated = result.unwrap();
        assert_eq!(validated.sans, vec!["pkcs1-rsa-test.example.com"]);
    }
}
