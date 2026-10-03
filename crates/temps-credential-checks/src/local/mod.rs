// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Local expiry inspection of structured credentials: X.509 certificates, SSH
//! certificates, OpenPGP keys, kubeconfigs and JWTs.
//!
//! Nothing here performs I/O. A value never leaves the host, no file path inside
//! a value is followed and no process is spawned, so no destination policy
//! applies to local checks. A stored value never changes between checks, so only
//! time-dependent properties are reported: expiry, not-yet-valid and revocation.
//!
//! A value is normalized into a few candidate views (as stored, with literal
//! `\n` escapes expanded, base64-decoded); every reader runs on each view and
//! the first view that yields expiring items wins.

mod armor;
mod jwt;
mod kubeconfig;
mod openpgp;
mod ssh;
mod x509;

use crate::verification::{
    expiry_finding, overall_status, valid_warning_days, CheckStatus, Finding, VerificationError,
    VerificationResult, INVALID_WARNING_DAYS,
};
use base64::Engine;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `automatic_provider` recorded for checks created from a detected expiring item.
pub const LOCAL_PROVIDER: &str = "local_expiry";
/// Bounds parsing work and allocations; real chains, keyrings and kubeconfigs
/// are a few kilobytes.
pub const MAX_LOCAL_INPUT_BYTES: usize = 65_536;
const MAX_ARTIFACTS: usize = 16;
const MAX_LABEL_CHARS: usize = 128;

/// The formats local checks read. Adding one needs no migration or new check kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    X509Certificate,
    SshCertificate,
    OpenpgpKey,
    Jwt,
}
impl ArtifactKind {
    fn count(self, n: usize) -> String {
        let (one, many) = match self {
            ArtifactKind::X509Certificate => ("certificate", "certificates"),
            ArtifactKind::SshCertificate => ("SSH certificate", "SSH certificates"),
            ArtifactKind::OpenpgpKey => ("OpenPGP key", "OpenPGP keys"),
            ArtifactKind::Jwt => ("JWT", "JWTs"),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    }
}

/// One expiring item found in a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExpiringArtifact {
    pub kind: ArtifactKind,
    /// Names the item for people, for example "Certificate 'svc.example.test'"
    /// or "OpenPGP key 0x0123456789ABCDEF". Never secret material and never
    /// token claims.
    pub label: String,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub not_before: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: DateTime<Utc>,
}

/// Everything local inspection learned about a value.
#[derive(Debug, Clone, Default)]
pub struct Inspection {
    /// Ordered by expiry, earliest first.
    pub artifacts: Vec<ExpiringArtifact>,
    /// Problems with recognized content: malformed blocks, revoked keys,
    /// short-lived tokens. Messages never contain secret material.
    pub findings: Vec<Finding>,
}

/// Reads every supported format in the value. Bounded and free of I/O.
pub fn inspect(value: &str) -> Inspection {
    if value.len() > MAX_LOCAL_INPUT_BYTES {
        return Inspection {
            artifacts: vec![],
            findings: vec![error(
                "credential_too_large",
                "The value exceeds the 64 KiB local inspection limit.".into(),
            )],
        };
    }
    let mut fallback = None;
    for view in views(value) {
        let found = match &view {
            View::Text(text) => inspect_text(text),
            View::Binary(bytes) => inspect_binary(bytes),
        };
        if !found.artifacts.is_empty() {
            return finish(found);
        }
        if fallback.is_none() && !found.findings.is_empty() {
            fallback = Some(found);
        }
    }
    fallback.map(finish).unwrap_or_default()
}

/// True when the value holds at least one item with an expiry. Used for
/// automatic detection, so values without one never create noise checks.
pub fn has_expiring_artifact(value: &str) -> bool {
    !inspect(value).artifacts.is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalCheckSpec {
    /// Warn when any expiring item in the value is this close to expiry.
    #[serde(default = "default_warning_days")]
    pub warning_days: Vec<u16>,
}
impl Default for LocalCheckSpec {
    fn default() -> Self {
        Self {
            warning_days: default_warning_days(),
        }
    }
}
fn default_warning_days() -> Vec<u16> {
    vec![30, 7, 1]
}

/// Takes no transport, so a local check cannot perform network I/O.
#[derive(Debug, Clone)]
pub struct LocalVerifier {
    spec: LocalCheckSpec,
}
impl LocalVerifier {
    pub fn new(spec: LocalCheckSpec) -> Result<Self, VerificationError> {
        if !valid_warning_days(&spec.warning_days) {
            return Err(VerificationError::Configuration {
                reason: INVALID_WARNING_DAYS,
            });
        }
        Ok(Self { spec })
    }
    pub fn spec(&self) -> &LocalCheckSpec {
        &self.spec
    }
    /// Never fails: a value without readable expiring items is itself the finding.
    pub fn verify(&self, credential: Option<&str>, now: DateTime<Utc>) -> VerificationResult {
        let inspection = inspect(credential.unwrap_or_default());
        let mut findings = Vec::new();
        if !inspection.artifacts.is_empty() {
            findings.push(Finding {
                code: "artifacts_found".into(),
                status: CheckStatus::Healthy,
                message: format!("Found {}.", summary(&inspection.artifacts)),
            });
        }
        findings.extend(inspection.findings);
        if findings.is_empty() {
            findings.push(error(
                "credential_not_inspectable",
                "The value contains no certificate, SSH certificate, OpenPGP key, kubeconfig or JWT with an expiry.".into(),
            ));
        }
        for artifact in &inspection.artifacts {
            if let Some(not_before) = artifact.not_before.filter(|t| *t > now) {
                findings.push(error(
                    "not_yet_valid",
                    format!(
                        "{} is not valid until {}.",
                        artifact.label,
                        not_before.to_rfc3339_opts(SecondsFormat::Secs, true)
                    ),
                ));
            }
            findings.push(expiry_finding(
                &artifact.label,
                artifact.expires_at,
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

/// "2 certificates and 1 JWT".
fn summary(artifacts: &[ExpiringArtifact]) -> String {
    let mut counts: Vec<(ArtifactKind, usize)> = Vec::new();
    for artifact in artifacts {
        match counts.iter_mut().find(|(kind, _)| *kind == artifact.kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((artifact.kind, 1)),
        }
    }
    let parts: Vec<String> = counts.iter().map(|(kind, n)| kind.count(*n)).collect();
    match parts.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// What the readers found in one view of the value.
#[derive(Debug, Default)]
struct Found {
    artifacts: Vec<ExpiringArtifact>,
    findings: Vec<Finding>,
}
impl Found {
    fn extend(&mut self, other: Found) {
        self.artifacts.extend(other.artifacts);
        self.findings.extend(other.findings);
    }
    fn problem(code: &str, message: String) -> Self {
        Self {
            artifacts: vec![],
            findings: vec![error(code, message)],
        }
    }
}

fn finish(mut found: Found) -> Inspection {
    if found.artifacts.len() > MAX_ARTIFACTS {
        return Inspection {
            artifacts: vec![],
            findings: vec![error(
                "too_many_items",
                format!("The value contains more than {MAX_ARTIFACTS} expiring items."),
            )],
        };
    }
    found.artifacts.sort_by_key(|artifact| artifact.expires_at);
    Inspection {
        artifacts: found.artifacts,
        findings: found.findings,
    }
}

enum View {
    Text(String),
    Binary(Vec<u8>),
}

/// The encodings structured credentials commonly arrive in through variables:
/// as stored (including indented blocks pasted from YAML), single-line values
/// with literal `\n` escapes, and base64 of either text or binary (DER, OpenPGP).
fn views(value: &str) -> Vec<View> {
    let mut views = Vec::with_capacity(4);
    push_text_views(&mut views, value);
    if let Some(bytes) = decode_base64(value) {
        // Binary formats can happen to be valid UTF-8, so try both readings.
        if let Ok(text) = std::str::from_utf8(&bytes) {
            push_text_views(&mut views, text);
        }
        views.push(View::Binary(bytes));
    }
    views
}
fn push_text_views(views: &mut Vec<View>, value: &str) {
    let text = value.replace("\r\n", "\n");
    // Base64, PEM bodies and YAML scalars never contain a backslash.
    let unescaped = text
        .contains("\\n")
        .then(|| text.replace("\\r\\n", "\n").replace("\\n", "\n"));
    views.push(View::Text(text));
    if let Some(unescaped) = unescaped {
        views.push(View::Text(unescaped));
    }
}

fn inspect_text(text: &str) -> Found {
    let mut found = x509::from_pem_text(text, None);
    found.extend(openpgp::from_armored_text(text));
    found.extend(ssh::from_text(text));
    found.extend(kubeconfig::from_text(text));
    found.extend(jwt::from_value(text, "JWT"));
    found
}
fn inspect_binary(bytes: &[u8]) -> Found {
    let mut found = Found::default();
    if let Ok(artifacts) = x509::from_der(bytes, None) {
        found.artifacts.extend(artifacts);
    }
    found.extend(openpgp::from_binary(bytes));
    found
}

/// Decodes a value that is base64 as a whole (standard or URL-safe alphabet,
/// padded or not, wrapped or not).
fn decode_base64(value: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let compact: String = value.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    // Shorter values cannot hold any supported format.
    if compact.len() < 32 {
        return None;
    }
    [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|engine| engine.decode(&compact).ok())
}

/// Labels come from the value itself (certificate subjects, kubeconfig entry
/// names, SSH key IDs): strip control characters and bound their length.
fn label_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect();
    cleaned.trim().to_owned()
}

fn timestamp(seconds: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(seconds, 0)
}

fn error(code: &str, message: String) -> Finding {
    Finding {
        code: code.into(),
        status: CheckStatus::Error,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-21T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }
    pub(super) fn at(date: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("{date}T00:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }
    pub(super) fn verify(value: &str) -> VerificationResult {
        LocalVerifier::new(LocalCheckSpec::default())
            .unwrap()
            .verify(Some(value), now())
    }
    fn codes(result: &VerificationResult) -> Vec<&str> {
        result.findings.iter().map(|f| f.code.as_str()).collect()
    }

    #[test]
    fn values_without_expiring_items_are_not_inspectable() {
        for value in [
            "",
            "ghp_not_a_structured_credential",
            "postgres://app:password@db.internal.test:5432/app",
            "just some words that are not a credential at all",
        ] {
            let result = verify(value);
            assert_eq!(result.status, CheckStatus::Error);
            assert_eq!(codes(&result), ["credential_not_inspectable"], "{value}");
            assert!(!has_expiring_artifact(value));
        }
        let result = LocalVerifier::new(LocalCheckSpec::default())
            .unwrap()
            .verify(None, now());
        assert_eq!(codes(&result), ["credential_not_inspectable"]);
    }

    #[test]
    fn oversized_values_are_reported_without_parsing() {
        let result = verify(&"A".repeat(MAX_LOCAL_INPUT_BYTES + 1));
        assert_eq!(codes(&result), ["credential_too_large"]);
    }

    #[test]
    fn summary_counts_each_kind() {
        let artifact = |kind| ExpiringArtifact {
            kind,
            label: "x".into(),
            not_before: None,
            expires_at: now(),
        };
        assert_eq!(
            summary(&[artifact(ArtifactKind::X509Certificate)]),
            "1 certificate"
        );
        assert_eq!(
            summary(&[
                artifact(ArtifactKind::X509Certificate),
                artifact(ArtifactKind::X509Certificate),
                artifact(ArtifactKind::Jwt),
                artifact(ArtifactKind::OpenpgpKey),
            ]),
            "2 certificates, 1 JWT and 1 OpenPGP key"
        );
    }

    #[test]
    fn labels_drop_control_characters_and_are_bounded() {
        assert_eq!(label_text("  ci\u{1b}[31m\n  "), "ci[31m");
        assert_eq!(label_text(&"n".repeat(500)).len(), MAX_LABEL_CHARS);
    }

    #[test]
    fn rejects_invalid_warning_thresholds() {
        for days in [vec![], vec![0], vec![366], vec![1; 9]] {
            assert!(LocalVerifier::new(LocalCheckSpec { warning_days: days }).is_err());
        }
        assert!(LocalVerifier::new(LocalCheckSpec {
            warning_days: vec![14]
        })
        .is_ok());
        let parsed: LocalCheckSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, LocalCheckSpec::default());
    }

    #[test]
    fn random_bytes_never_panic() {
        // Deterministic xorshift so failures reproduce.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        for size in [1, 7, 64, 513, 4096] {
            for _ in 0..64 {
                let bytes: Vec<u8> = (0..size)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state as u8
                    })
                    .collect();
                let text = String::from_utf8_lossy(&bytes);
                inspect(&text);
                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                inspect(&encoded);
                for prefix in [
                    "-----BEGIN CERTIFICATE-----\n",
                    "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\n",
                    "ssh-ed25519-cert-v01@openssh.com ",
                    "apiVersion: v1\nkind: Config\nusers:\n- name: ci\n  user:\n    token: ",
                    "eyJhbGciOiJIUzI1NiJ9.",
                ] {
                    inspect(&format!("{prefix}{encoded}"));
                }
            }
        }
    }
}
