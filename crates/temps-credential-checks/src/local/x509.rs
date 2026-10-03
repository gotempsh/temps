// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! X.509 certificates, as PEM blocks or DER.

use super::{armor, error, label_text, timestamp, ArtifactKind, ExpiringArtifact, Found};
use base64::Engine;
use x509_parser::certificate::X509Certificate;

const LABELS: [&str; 3] = ["CERTIFICATE", "X509 CERTIFICATE", "TRUSTED CERTIFICATE"];

/// Reads every certificate block. Private keys and parameters bundled
/// alongside are skipped without being decoded. `context` names where the
/// certificates came from, such as "Kubeconfig user 'ci' client certificate".
pub(super) fn from_pem_text(text: &str, context: Option<&str>) -> Found {
    let noun = context.unwrap_or("Certificate");
    let mut found = Found::default();
    let mut position = 0;
    for block in armor::blocks(text) {
        if !LABELS.contains(&block.label) {
            continue;
        }
        position += 1;
        let invalid = |reason: &str| {
            error(
                "certificate_invalid",
                format!("{noun} block {position} {reason}."),
            )
        };
        if !block.terminated {
            found.findings.push(invalid("is not terminated"));
            continue;
        }
        let Ok(der) = base64::engine::general_purpose::STANDARD.decode(&block.body) else {
            found.findings.push(invalid("is not valid base64"));
            continue;
        };
        match artifact(&der, noun) {
            Ok((artifact, _)) => found.artifacts.push(artifact),
            Err(()) => found.findings.push(invalid("could not be parsed as X.509")),
        }
    }
    found
}

/// DER, possibly several certificates back to back. Errs when the bytes are
/// not certificates, so the caller decides whether that is a problem.
pub(super) fn from_der(der: &[u8], context: Option<&str>) -> Result<Vec<ExpiringArtifact>, ()> {
    let noun = context.unwrap_or("Certificate");
    let mut artifacts = Vec::new();
    let mut rest = der;
    while !rest.is_empty() && artifacts.len() <= super::MAX_ARTIFACTS {
        let (artifact, remaining) = artifact(rest, noun)?;
        artifacts.push(artifact);
        rest = remaining;
    }
    if artifacts.is_empty() {
        return Err(());
    }
    Ok(artifacts)
}

fn artifact<'a>(der: &'a [u8], noun: &str) -> Result<(ExpiringArtifact, &'a [u8]), ()> {
    // Every certificate is a DER SEQUENCE; reject anything else cheaply.
    if der.first() != Some(&0x30) {
        return Err(());
    }
    let (rest, certificate) = x509_parser::parse_x509_certificate(der).map_err(|_| ())?;
    let validity = certificate.validity();
    let not_before = timestamp(validity.not_before.timestamp()).ok_or(())?;
    let expires_at = timestamp(validity.not_after.timestamp()).ok_or(())?;
    Ok((
        ExpiringArtifact {
            kind: ArtifactKind::X509Certificate,
            label: format!("{noun} '{}'", common_name(&certificate)),
            not_before: Some(not_before),
            expires_at,
        },
        rest,
    ))
}

fn common_name(certificate: &X509Certificate<'_>) -> String {
    let subject = certificate.subject();
    let name = subject
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(label_text)
        .unwrap_or_else(|| label_text(&subject.to_string()));
    if name.is_empty() {
        "unnamed".into()
    } else {
        name
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::verify;
    use super::super::*;
    use crate::verification::{CheckStatus, VerificationResult};

    pub(in crate::local) fn certificate(
        name: &str,
        not_before: (i32, u8, u8),
        not_after: (i32, u8, u8),
    ) -> rcgen::Certificate {
        let mut params = rcgen::CertificateParams::new(vec![name.to_owned()]).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        params.not_before = rcgen::date_time_ymd(not_before.0, not_before.1, not_before.2);
        params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);
        let key = rcgen::KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap()
    }
    pub(in crate::local) fn certificate_pem(
        name: &str,
        not_before: (i32, u8, u8),
        not_after: (i32, u8, u8),
    ) -> String {
        certificate(name, not_before, not_after).pem()
    }
    pub(in crate::local) fn valid_until(not_after: (i32, u8, u8)) -> String {
        certificate_pem("leaf.example.test", (2026, 1, 1), not_after)
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
            assert_eq!(result.findings[0].code, "artifacts_found");
            assert_eq!(result.findings[0].message, "Found 1 certificate.");
        }
    }

    #[test]
    fn messages_name_the_certificate_and_use_utc_dates() {
        let result = verify(&valid_until((2026, 9, 28)));
        let message = &result.findings.last().unwrap().message;
        assert!(
            message.starts_with("Certificate 'leaf.example.test' expires"),
            "{message}"
        );
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
            .any(|f| f.code == "not_yet_valid" && f.message.contains("staged")));
    }

    #[test]
    fn every_chain_member_is_reported_earliest_first() {
        let chain = format!(
            "{}{}",
            valid_until((2027, 6, 1)),
            certificate_pem("Intermediate CA", (2025, 1, 1), (2026, 9, 25))
        );
        let result = verify(&chain);
        assert_eq!(result.findings[0].message, "Found 2 certificates.");
        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(result.findings[1].code, "expires_within_7_days");
        assert!(result.findings[1].message.contains("Intermediate CA"));
        assert_eq!(result.findings[2].code, "expiration_healthy");
    }

    #[test]
    fn accepts_common_variable_encodings() {
        use base64::engine::general_purpose::STANDARD;
        let pem = valid_until((2027, 1, 1));
        let base64 = STANDARD.encode(&pem);
        let wrapped_base64 = base64
            .as_bytes()
            .chunks(76)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let escaped = pem.trim_end().replace('\n', "\\n");
        let escaped_with_newline = format!("{escaped}\n");
        let indented = pem
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let crlf = pem.replace('\n', "\r\n");
        let der =
            STANDARD.encode(certificate("leaf.example.test", (2026, 1, 1), (2027, 1, 1)).der());
        for value in [
            &pem,
            &base64,
            &wrapped_base64,
            &escaped,
            &escaped_with_newline,
            &indented,
            &crlf,
            &der,
        ] {
            assert_eq!(expiry_code(&verify(value)), "expiration_healthy", "{value}");
            assert!(has_expiring_artifact(value));
        }
    }

    #[test]
    fn bundled_private_keys_are_ignored_and_never_echoed() {
        let key = rcgen::KeyPair::generate().unwrap().serialize_pem();
        // A legacy encrypted key carries RFC 1421 headers inside its block.
        let legacy = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,0011223344556677\n\nbm90IHJlYWw=\n-----END RSA PRIVATE KEY-----\n";
        let bundle = format!("{}{key}{legacy}", valid_until((2027, 1, 1)));
        let result = verify(&bundle);
        assert_eq!(result.status, CheckStatus::Healthy);
        assert_eq!(result.findings[0].message, "Found 1 certificate.");
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("PRIVATE"));
        assert!(!serialized.contains(key.lines().nth(1).unwrap()));
    }

    #[test]
    fn malformed_and_excessive_certificates_are_reported() {
        for (value, code) in [
            (
                "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n".to_owned(),
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
            (valid_until((2027, 1, 1)).repeat(17), "too_many_items"),
            (
                rcgen::KeyPair::generate().unwrap().serialize_pem(),
                "credential_not_inspectable",
            ),
        ] {
            let result = verify(&value);
            assert_eq!(result.status, CheckStatus::Error);
            assert_eq!(result.findings.len(), 1, "{:?}", result.findings);
            assert_eq!(result.findings[0].code, code);
            assert!(!has_expiring_artifact(&value));
        }
    }

    #[test]
    fn a_malformed_block_beside_a_good_one_is_reported_not_hidden() {
        let value = format!(
            "{}-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
            valid_until((2027, 1, 1))
        );
        assert!(has_expiring_artifact(&value));
        let result = verify(&value);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result
            .findings
            .iter()
            .any(|f| f.code == "certificate_invalid"
                && f.message == "Certificate block 2 could not be parsed as X.509."));
    }

    #[test]
    fn fingerprint_is_stable_while_inside_one_threshold() {
        let a = verify(&valid_until((2026, 10, 10)));
        let b = verify(&valid_until((2026, 10, 12)));
        assert_eq!(a.fingerprint(), "expires_within_30_days");
        assert_eq!(a.fingerprint(), b.fingerprint());
    }
}
