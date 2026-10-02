// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Local X.509 inspection. Certificate checks never perform network I/O, so the
//! value never leaves the host and no destination policy applies to it.

use crate::verification::{
    expiry_finding, overall_status, valid_warning_days, CheckStatus, Finding, VerificationError,
    VerificationResult, INVALID_WARNING_DAYS,
};
use base64::Engine;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use x509_parser::{certificate::X509Certificate, pem::Pem};

/// `automatic_provider` recorded for checks created from a detected certificate.
pub const CERTIFICATE_PROVIDER: &str = "x509_certificate";
/// Bounds parsing work and allocations; real chains are a few kilobytes.
pub const MAX_CERTIFICATE_INPUT_BYTES: usize = 65_536;
const MAX_CERTIFICATES: usize = 16;
const MAX_LABEL_CHARS: usize = 128;
const PEM_HEADER: &str = "-----BEGIN CERTIFICATE-----";
/// `base64("-----BEGIN CERTIFICATE-----")`, for PEM stored base64-encoded.
const BASE64_PEM_HEADER: &str = "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0t";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CertificateCheckSpec {
    /// Warn when the earliest-expiring certificate in the value is this close to expiry.
    #[serde(default = "default_warning_days")]
    pub warning_days: Vec<u16>,
}
impl Default for CertificateCheckSpec {
    fn default() -> Self {
        Self {
            warning_days: default_warning_days(),
        }
    }
}
fn default_warning_days() -> Vec<u16> {
    vec![30, 7, 1]
}

#[derive(Debug, Clone)]
pub struct CertificateVerifier {
    spec: CertificateCheckSpec,
}
impl CertificateVerifier {
    pub fn new(spec: CertificateCheckSpec) -> Result<Self, VerificationError> {
        if !valid_warning_days(&spec.warning_days) {
            return Err(VerificationError::Configuration {
                reason: INVALID_WARNING_DAYS,
            });
        }
        Ok(Self { spec })
    }
    pub fn spec(&self) -> &CertificateCheckSpec {
        &self.spec
    }
    /// Never fails: a missing or malformed certificate is itself the reported finding.
    pub fn verify(&self, credential: Option<&str>, now: DateTime<Utc>) -> VerificationResult {
        let certificates = match inspect(credential.unwrap_or_default()) {
            Ok(certificates) => certificates,
            Err(finding) => {
                return VerificationResult {
                    status: finding.status.clone(),
                    findings: vec![finding],
                    checked_at: now,
                }
            }
        };
        let mut findings = vec![Finding {
            code: "certificate_parsed".into(),
            status: CheckStatus::Healthy,
            message: format!("Found {} certificate(s).", certificates.len()),
        }];
        if let Some(pending) = certificates
            .iter()
            .filter(|c| c.not_before > now)
            .min_by_key(|c| c.not_before)
        {
            findings.push(Finding {
                code: "certificate_not_yet_valid".into(),
                status: CheckStatus::Error,
                message: format!(
                    "Certificate '{}' is not valid until {}.",
                    pending.label,
                    pending
                        .not_before
                        .to_rfc3339_opts(SecondsFormat::Secs, true)
                ),
            });
        }
        // A chain is only as valid as its earliest-expiring member, often an intermediate.
        if let Some(earliest) = certificates.iter().min_by_key(|c| c.not_after) {
            findings.push(expiry_finding(
                &format!("Certificate '{}'", earliest.label),
                earliest.not_after,
                now,
                &self.spec.warning_days,
            ));
        }
        VerificationResult {
            status: overall_status(&findings),
            findings,
            checked_at: now,
        }
    }
}

/// True when the value holds at least one parseable X.509 certificate.
/// Used for automatic detection, so malformed blocks never create noise checks.
pub fn contains_certificate(value: &str) -> bool {
    inspect(value).is_ok()
}

struct InspectedCertificate {
    label: String,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
}

fn inspect(value: &str) -> Result<Vec<InspectedCertificate>, Finding> {
    if value.len() > MAX_CERTIFICATE_INPUT_BYTES {
        return Err(error_finding(
            "certificate_too_large",
            "The value exceeds the 64 KiB certificate inspection limit.".into(),
        ));
    }
    let not_found = || {
        error_finding(
            "certificate_not_found",
            "The value does not contain a PEM-encoded certificate.".into(),
        )
    };
    let text = pem_text(value).ok_or_else(not_found)?;
    let mut certificates = Vec::new();
    for block in Pem::iter_from_buffer(text.as_bytes()) {
        let block = block.map_err(|_| {
            error_finding(
                "certificate_invalid",
                format!(
                    "PEM block {} is malformed.",
                    certificates.len().saturating_add(1)
                ),
            )
        })?;
        // Private keys and parameters bundled alongside are skipped, never inspected.
        if block.label != "CERTIFICATE" {
            continue;
        }
        if certificates.len() == MAX_CERTIFICATES {
            return Err(error_finding(
                "certificate_chain_too_long",
                format!("The value contains more than {MAX_CERTIFICATES} certificates."),
            ));
        }
        let position = certificates.len() + 1;
        let unparseable = || {
            error_finding(
                "certificate_invalid",
                format!("Certificate {position} could not be parsed as X.509."),
            )
        };
        let certificate = block.parse_x509().map_err(|_| unparseable())?;
        let validity = certificate.validity();
        let (Some(not_before), Some(not_after)) = (
            DateTime::from_timestamp(validity.not_before.timestamp(), 0),
            DateTime::from_timestamp(validity.not_after.timestamp(), 0),
        ) else {
            return Err(unparseable());
        };
        certificates.push(InspectedCertificate {
            label: label(&certificate),
            not_before,
            not_after,
        });
    }
    if certificates.is_empty() {
        return Err(not_found());
    }
    Ok(certificates)
}

/// Normalizes the encodings certificates commonly arrive in through variables:
/// raw PEM, base64-wrapped PEM, single-line values with literal `\n` escapes,
/// and indented blocks pasted from YAML.
fn pem_text(value: &str) -> Option<String> {
    let text = if value.contains(PEM_HEADER) {
        value.to_owned()
    } else if value.trim_start().starts_with(BASE64_PEM_HEADER) {
        let compact: String = value.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(compact)
            .ok()?;
        String::from_utf8(decoded)
            .ok()
            .filter(|text| text.contains(PEM_HEADER))?
    } else {
        return None;
    };
    let text = if !text.contains('\n') && text.contains("\\n") {
        text.replace("\\n", "\n")
    } else {
        text
    };
    Some(text.lines().map(str::trim).collect::<Vec<_>>().join("\n"))
}

fn label(certificate: &X509Certificate<'_>) -> String {
    let subject = certificate.subject();
    let name = subject
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| subject.to_string());
    if name.trim().is_empty() {
        return "unnamed certificate".into();
    }
    name.chars().take(MAX_LABEL_CHARS).collect()
}

fn error_finding(code: &str, message: String) -> Finding {
    Finding {
        code: code.into(),
        status: CheckStatus::Error,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-21T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }
    fn certificate_pem(name: &str, not_before: (i32, u8, u8), not_after: (i32, u8, u8)) -> String {
        let mut params = rcgen::CertificateParams::new(vec![name.to_owned()]).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        params.not_before = rcgen::date_time_ymd(not_before.0, not_before.1, not_before.2);
        params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);
        let key = rcgen::KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap().pem()
    }
    fn valid_until(not_after: (i32, u8, u8)) -> String {
        certificate_pem("leaf.example.test", (2026, 1, 1), not_after)
    }
    fn verify(value: &str) -> VerificationResult {
        CertificateVerifier::new(CertificateCheckSpec::default())
            .unwrap()
            .verify(Some(value), now())
    }
    fn expiry_code(result: &VerificationResult) -> &str {
        &result.findings.last().unwrap().code
    }

    #[test]
    fn expiry_thresholds_match_http_check_codes() {
        for (not_after, code, status) in [
            ((2026, 9, 21), "expired", CheckStatus::Error),
            ((2026, 9, 22), "expires_within_1_days", CheckStatus::Warning),
            ((2026, 9, 28), "expires_within_7_days", CheckStatus::Warning),
            (
                (2026, 10, 20),
                "expires_within_30_days",
                CheckStatus::Warning,
            ),
            ((2027, 1, 1), "expiration_healthy", CheckStatus::Healthy),
        ] {
            let result = verify(&valid_until(not_after));
            assert_eq!(expiry_code(&result), code, "not_after={not_after:?}");
            assert_eq!(result.status, status, "not_after={not_after:?}");
            assert_eq!(result.findings[0].code, "certificate_parsed");
        }
    }

    #[test]
    fn messages_name_the_certificate_and_use_utc_dates() {
        let result = verify(&valid_until((2026, 9, 28)));
        let message = &result.findings.last().unwrap().message;
        assert!(message.contains("leaf.example.test"), "{message}");
        assert!(message.contains("2026-09-28T00:00:00Z"), "{message}");
    }

    #[test]
    fn not_yet_valid_certificates_are_errors() {
        let result = verify(&certificate_pem(
            "staged.example.test",
            (2026, 10, 1),
            (2027, 10, 1),
        ));
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result
            .findings
            .iter()
            .any(|f| f.code == "certificate_not_yet_valid" && f.message.contains("staged")));
    }

    #[test]
    fn chain_expiry_follows_the_earliest_certificate() {
        let chain = format!(
            "{}{}",
            valid_until((2027, 6, 1)),
            certificate_pem("Intermediate CA", (2025, 1, 1), (2026, 9, 25))
        );
        let result = verify(&chain);
        assert_eq!(result.findings[0].message, "Found 2 certificate(s).");
        assert_eq!(expiry_code(&result), "expires_within_7_days");
        assert!(result
            .findings
            .last()
            .unwrap()
            .message
            .contains("Intermediate CA"));
    }

    #[test]
    fn accepts_common_variable_encodings() {
        let pem = valid_until((2027, 1, 1));
        let base64 = base64::engine::general_purpose::STANDARD.encode(&pem);
        let wrapped_base64 = base64
            .as_bytes()
            .chunks(76)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let escaped = pem.trim_end().replace('\n', "\\n");
        let indented = pem
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let crlf = pem.replace('\n', "\r\n");
        for value in [&pem, &base64, &wrapped_base64, &escaped, &indented, &crlf] {
            assert_eq!(expiry_code(&verify(value)), "expiration_healthy", "{value}");
            assert!(contains_certificate(value));
        }
    }

    #[test]
    fn bundled_private_keys_are_ignored_and_never_echoed() {
        let key = rcgen::KeyPair::generate().unwrap().serialize_pem();
        let bundle = format!("{}{key}", valid_until((2027, 1, 1)));
        let result = verify(&bundle);
        assert_eq!(result.status, CheckStatus::Healthy);
        assert_eq!(result.findings[0].message, "Found 1 certificate(s).");
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("PRIVATE"));
        assert!(!serialized.contains(key.lines().nth(1).unwrap()));
    }

    #[test]
    fn missing_malformed_and_oversized_values_are_reported() {
        for (value, code) in [
            (String::new(), "certificate_not_found"),
            ("ghp_not_a_certificate".into(), "certificate_not_found"),
            (
                rcgen::KeyPair::generate().unwrap().serialize_pem(),
                "certificate_not_found",
            ),
            (
                "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n".into(),
                "certificate_invalid",
            ),
            (
                "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".into(),
                "certificate_invalid",
            ),
            (
                "-----BEGIN CERTIFICATE-----\nAAAA\n".into(),
                "certificate_invalid",
            ),
            (
                "A".repeat(MAX_CERTIFICATE_INPUT_BYTES + 1),
                "certificate_too_large",
            ),
            (
                valid_until((2027, 1, 1)).repeat(17),
                "certificate_chain_too_long",
            ),
        ] {
            let result = verify(&value);
            assert_eq!(result.status, CheckStatus::Error);
            assert_eq!(result.findings.len(), 1);
            assert_eq!(result.findings[0].code, code);
            assert!(!contains_certificate(&value));
        }
        let result = CertificateVerifier::new(CertificateCheckSpec::default())
            .unwrap()
            .verify(None, now());
        assert_eq!(result.findings[0].code, "certificate_not_found");
    }

    #[test]
    fn fingerprint_is_stable_while_inside_one_threshold() {
        let a = verify(&valid_until((2026, 10, 10)));
        let b = verify(&valid_until((2026, 10, 12)));
        assert_eq!(a.fingerprint(), "expires_within_30_days");
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn rejects_invalid_warning_thresholds() {
        for days in [vec![], vec![0], vec![366], vec![1; 9]] {
            assert!(CertificateVerifier::new(CertificateCheckSpec { warning_days: days }).is_err());
        }
        assert!(CertificateVerifier::new(CertificateCheckSpec {
            warning_days: vec![14]
        })
        .is_ok());
        let parsed: CertificateCheckSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, CertificateCheckSpec::default());
    }
}
