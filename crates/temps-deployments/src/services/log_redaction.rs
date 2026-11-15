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

    let mut values = Vec::new();
    for secret in secrets {
        if secret.len() >= MIN_KNOWN_SECRET_LEN {
            values.push(secret.as_str());
        }
        // A bounded log tail can start inside a configured multiline secret.
        // Its surviving lines must be removed even without the complete value.
        if secret.contains(['\r', '\n']) {
            values.extend(secret.lines().filter(|line| !line.is_empty()));
        }
    }
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

#[allow(
    clippy::expect_used,
    reason = "every pattern is a string literal; each is compiled and exercised by the \
              unit tests below, so a typo fails `cargo test`, never production"
)]
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
        // Fail closed when a bounded tail starts inside an unconfigured key.
        SecretPattern {
            regex: Regex::new(r"(?s)\A.*-----END [A-Z ]*PRIVATE KEY-----").expect("valid regex"),
        },
        // An incomplete key printed before a build stops has no END marker.
        SecretPattern {
            regex: Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*\z").expect("valid regex"),
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

/// Remove identifying addresses and repository/path references from outbound reports.
/// This deliberately favors privacy over retaining paths in diagnostic output.
pub fn redact_report_identifiers(text: &str) -> String {
    redact_report_identifiers_with_pattern(text, None)
}

/// Match every identifier against the same input, then merge overlapping spans.
/// Sequential replacement can destroy a larger match and expose its suffix.
pub(super) fn redact_report_identifiers_with_pattern(text: &str, extra: Option<&Regex>) -> String {
    let mut ranges = Vec::new();
    for pattern in IDENTIFIER_PATTERNS.iter().chain(extra) {
        ranges.extend(
            pattern
                .find_iter(text)
                .map(|matched| (matched.start(), matched.end())),
        );
    }
    ranges.extend(IPV6_PATTERN.find_iter(text).filter_map(|matched| {
        matched
            .as_str()
            .trim_matches(['[', ']'])
            .parse::<std::net::Ipv6Addr>()
            .ok()
            .map(|_| (matched.start(), matched.end()))
    }));
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(previous) = merged.last_mut() {
            if start < previous.1 {
                previous.1 = previous.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    let mut safe = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, end) in merged {
        safe.push_str(&text[cursor..start]);
        safe.push_str("[redacted]");
        cursor = end;
    }
    safe.push_str(&text[cursor..]);
    safe
}

#[allow(
    clippy::expect_used,
    reason = "literal patterns exercised by adjacent tests"
)]
static IDENTIFIER_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Whole URLs first, including scp-style Git remotes.
        r#"(?i)(?:[a-z][a-z0-9+.-]*://|git@)[^\s<>"']+"#,
        // Emails, bare domains (including internal hostnames), IPv4.
        r#"[^\s@<>"']+@[^\s@<>"']+"#,
        r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b",
        r"(?i)\b(?:[\p{L}\p{N}_-]+\.)+[\p{L}][\p{L}\p{N}_-]*\b",
        // Commit, trace and request identifiers can identify a repository or captured operation.
        r"(?i)\b[0-9a-f]{16,64}\b",
        r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
        // Bare owner/repository, absolute paths and nested checkout paths.
        r"(?:[\p{L}\p{N}_.@~-]+)?(?:/[\p{L}\p{N}_.@~-]+)+",
        r#"(?i)(?:[a-z]:\\|\\\\)[^\s<>"']+"#,
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid identifier pattern"))
    .collect()
});

// Validate candidates with the standard parser so timestamps are retained.
#[allow(
    clippy::expect_used,
    reason = "literal IPv6 candidate pattern exercised by adjacent tests"
)]
static IPV6_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[?(?:[a-f0-9]{0,4}:){2,}[a-f0-9:.]*\]?")
        .expect("valid IPv6 candidate pattern")
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_multiline_secrets_are_withheld() {
        let secret = "head-value\nsecret-body\nx".to_string();
        assert_eq!(
            redact_known_secrets("secret-body\nx\nfailed", &[secret]),
            "***\n***\nfailed"
        );
    }

    #[test]
    fn key_material_is_removed_when_the_tail_loses_its_begin_marker() {
        let material = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=";
        let text = format!(
            "-----BEGIN PRIVATE KEY-----\n{}-----END PRIVATE KEY-----\n{}failure",
            format!("{material}\n").repeat(26),
            "progress\n".repeat(1990)
        );
        let tail = text
            .lines()
            .rev()
            .take(2000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!tail.contains("BEGIN PRIVATE KEY"));
        assert!(tail.contains(material));
        let safe = redact_common_secret_patterns(&tail);
        assert!(!safe.contains(material), "{safe}");
        assert!(safe.ends_with("failure"));
        assert!(!redact_common_secret_patterns(&format!(
            "-----BEGIN PRIVATE KEY-----\n{material}"
        ))
        .contains(material));
    }

    #[test]
    fn redacts_commit_trace_and_request_identifiers() {
        let safe = redact_report_identifiers("commit 1111111111111111111111111111111111111111 request 11111111-2222-4333-8444-555555555555");
        assert_eq!(safe, "commit [redacted] request [redacted]");
    }

    #[test]
    fn redacts_unicode_addresses_and_windows_checkout_paths() {
        let text = r"contact opérateur@exemple.invalid hôte.exemple.invalid C:\work\sample-org\sample-repo";
        let safe = redact_report_identifiers(text);
        for identifying in ["opérateur", "exemple", "sample-org", "sample-repo"] {
            assert!(!safe.contains(identifying), "{safe}");
        }
    }

    #[test]
    fn preserves_versions_and_failure_diagnostics() {
        let text = "Temps version 0.1.0-beta.62: 12:34:56 build failed with exit code 127";
        assert_eq!(redact_report_identifiers(text), text);
    }

    #[test]
    fn outbound_reports_remove_addresses_and_repository_paths() {
        let text = "clone https://github.com/sample-org/sample-repo.git git@forge.example:sample-org/sample-repo.git sample-org/sample-repo /workspace/sample-repo contact dev@example.invalid 192.0.2.10 [2001:db8::1] ::1 api.internal";
        let safe = redact_report_identifiers(text);
        for identifying in [
            "sample-org",
            "sample-repo",
            "dev@",
            "192.0.2",
            "2001",
            "::1",
            "api.internal",
        ] {
            assert!(!safe.contains(identifying), "{safe}");
        }
        assert!(safe.contains("clone"));
    }

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
