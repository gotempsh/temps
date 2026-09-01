// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Redacts secrets from deployment logs before they're shown to a user as a
//! starting draft for a failure report (see [`super::failure_report_service`]),
//! or forwarded anywhere.
//!
//! Two independent passes, applied in order:
//! 1. [`redact_known_secrets`] — exact-match replacement of every literal
//!    value Temps has configured as an env var / secret / build arg for the
//!    deployment. This is the reliable pass: we know precisely which strings
//!    are secret.
//! 2. [`redact_common_secret_patterns`] — a heuristic regex pass for secret
//!    shapes we don't already know about (a token a build tool printed that
//!    isn't one of our own configured vars).

use std::sync::LazyLock;

use regex::Regex;

/// Known-secret values shorter than this are skipped. Real tokens/passwords
/// are almost always longer than this; without a floor, short configured
/// values (a port number, a boolean flag) would blank out unrelated numbers
/// and words throughout the log, making it useless to read.
const MIN_KNOWN_SECRET_LEN: usize = 6;

/// Replace every literal occurrence of a known secret value in `text` with
/// `***`. `secrets` is typically every value from a deployment's resolved
/// env vars, remote env vars, secrets, and build args (see
/// [`super::sensitive_envelope::read_sealed_optional`]).
pub fn redact_known_secrets(text: &str, secrets: &[String]) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut values: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        .filter(|s| s.len() >= MIN_KNOWN_SECRET_LEN)
        .collect();
    values.sort_unstable();
    values.dedup();
    // Longest first: if one known secret happens to be a substring of
    // another (e.g. a DB password that's also embedded in a connection-string
    // secret), redacting the longer one first keeps the replacement clean
    // instead of leaving `***word` fragments behind.
    values.sort_by_key(|s| std::cmp::Reverse(s.len()));

    let mut redacted = text.to_string();
    for value in values {
        redacted = redacted.replace(value, "***");
    }
    redacted
}

struct SecretPattern {
    regex: Regex,
}

static SECRET_PATTERNS: LazyLock<Vec<SecretPattern>> = LazyLock::new(|| {
    vec![
        // AWS access key IDs.
        SecretPattern {
            regex: Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("valid regex"),
        },
        // JWTs (header.payload.signature, base64url segments).
        SecretPattern {
            regex: Regex::new(r"\bey[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b")
                .expect("valid regex"),
        },
        // `Authorization: Bearer <token>` / `Bearer <token>` anywhere in output.
        SecretPattern {
            regex: Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}").expect("valid regex"),
        },
        // Basic-auth credentials embedded in a URL: scheme://user:pass@host
        SecretPattern {
            regex: Regex::new(r"://[^\s:/@]+:[^\s:/@]+@").expect("valid regex"),
        },
        // PEM-encoded private key blocks.
        SecretPattern {
            regex: Regex::new(
                r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            )
            .expect("valid regex"),
        },
    ]
});

/// Regex pass for secret shapes not covered by [`redact_known_secrets`] —
/// tokens a build tool printed that Temps never configured itself.
pub fn redact_common_secret_patterns(text: &str) -> String {
    let mut redacted = text.to_string();
    for pattern in SECRET_PATTERNS.iter() {
        redacted = pattern.regex.replace_all(&redacted, "***").into_owned();
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_secret_values() {
        let text = "Connecting with password hunter2secret and api key sk-live-abc123xyz";
        let secrets = vec!["hunter2secret".to_string(), "sk-live-abc123xyz".to_string()];
        let redacted = redact_known_secrets(text, &secrets);
        assert!(!redacted.contains("hunter2secret"));
        assert!(!redacted.contains("sk-live-abc123xyz"));
        assert!(redacted.contains("***"));
    }

    #[test]
    fn skips_short_values_to_avoid_over_redaction() {
        let text = "PORT=3000 and retries=5";
        let secrets = vec!["3000".to_string(), "5".to_string()];
        let redacted = redact_known_secrets(text, &secrets);
        assert_eq!(
            redacted, text,
            "short values below the floor must be left alone"
        );
    }

    #[test]
    fn longest_value_redacted_first_avoids_fragment_leak() {
        let text = "connection string is postgres://u:hunter2@host/db and password is hunter2";
        let secrets = vec![
            "hunter2".to_string(),
            "postgres://u:hunter2@host/db".to_string(),
        ];
        let redacted = redact_known_secrets(text, &secrets);
        assert!(!redacted.contains("hunter2"));
    }

    #[test]
    fn redacts_aws_access_key() {
        let text = "AWS_ACCESS_KEY_ID=AKIAABCDEFGHIJKLMNOP failed auth";
        let redacted = redact_common_secret_patterns(text);
        assert!(!redacted.contains("AKIAABCDEFGHIJKLMNOP"));
    }

    #[test]
    fn redacts_jwt() {
        let text =
            "token: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dGhpc2lzYXNpZ25hdHVyZQ end";
        let redacted = redact_common_secret_patterns(text);
        assert!(!redacted.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn redacts_bearer_token() {
        let text = "curl -H 'Authorization: Bearer sk_live_51H8xyzABC123' https://api.example.com";
        let redacted = redact_common_secret_patterns(text);
        assert!(!redacted.contains("sk_live_51H8xyzABC123"));
    }

    #[test]
    fn redacts_basic_auth_url() {
        let text = "cloning https://oauth2:ghp_abc123def456@github.com/org/repo.git";
        let redacted = redact_common_secret_patterns(text);
        assert!(!redacted.contains("ghp_abc123def456"));
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let text =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----";
        let redacted = redact_common_secret_patterns(text);
        assert!(!redacted.contains("MIIEpAIBAAKCAQEA"));
    }

    #[test]
    fn ordinary_log_text_is_untouched() {
        let text = "Step 3/8 : COPY . .\nSuccessfully built abc123\nBuild complete in 4.2s";
        assert_eq!(redact_common_secret_patterns(text), text);
        assert_eq!(redact_known_secrets(text, &[]), text);
    }
}
