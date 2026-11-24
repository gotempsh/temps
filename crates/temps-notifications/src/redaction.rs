// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Masking credentials out of notification text before any provider sees it.
//!
//! Notification messages are often built from raw error strings (a failed
//! backup, a failed clone), which can carry a git installation token in a
//! remote URL, an S3 secret key or an environment value. Every notification is
//! passed through [`mask_notification`] once, centrally, before it is stored
//! or delivered, so Slack, Discord, email, webhooks and Temps Cloud all get
//! the same masked text.
//!
//! The passes are conservative about ordinary prose: a token prefix only
//! matches at the start of a word (`tasks_` never matches `sk_`), and a
//! `key=value` pair is only masked when the key names a secret. Every pass is
//! idempotent: masking masked text changes nothing.

use crate::types::Notification;

pub(crate) const REDACTED: &str = "[redacted]";

/// Token formats recognised by their prefix. Matched case-insensitively and
/// only at a word boundary; the longest matching prefix wins.
const SECRET_PREFIXES: &[&str] = &[
    "bearer ",
    // Anthropic / OpenAI-style API keys.
    "sk-ant-",
    "sk-",
    // Stripe secret and restricted keys.
    "sk_live_",
    "rk_live_",
    // GitHub: personal, OAuth, app installation, user-to-server, refresh and
    // fine-grained personal tokens.
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    // GitLab: personal, runner and deploy tokens.
    "glpat-",
    "glrt-",
    "gldt-",
    // Slack tokens.
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
];

/// Keys whose value is a secret in `key=value` / `key: value` text. A key
/// also matches as the last segment of a longer identifier, so
/// `GITHUB_TOKEN=` and `x-api-key:` are caught as well.
const SECRET_KEYS: &[&str] = &[
    "access_key",
    "api_key",
    "apikey",
    "authorization",
    "client_secret",
    "credentials",
    "passwd",
    "password",
    "private_key",
    "secret",
    "secret_key",
    "token",
];

/// HTTP authorization schemes: in `Authorization: <scheme> <credential>` the
/// credential after the scheme word is masked too.
const AUTH_SCHEMES: &[&str] = &[
    "apikey",
    "basic",
    "bearer",
    "digest",
    "key",
    "negotiate",
    "ntlm",
    "token",
];

/// Mask credentials in a notification's title, message and metadata values.
/// A metadata value whose key itself names a secret is replaced outright.
pub(crate) fn mask_notification(notification: &mut Notification) {
    notification.title = redact_sensitive_text(&notification.title);
    notification.message = redact_sensitive_text(&notification.message);
    for (key, value) in notification.metadata.iter_mut() {
        *value = if is_secret_key(key) && !value.is_empty() {
            REDACTED.to_string()
        } else {
            redact_sensitive_text(value)
        };
    }
}

/// Mask every credential format this module recognises in `value`.
pub(crate) fn redact_sensitive_text(value: &str) -> String {
    let value = redact_url_credentials(value);
    let value = redact_secret_prefixes(&value);
    let value = redact_aws_access_key_ids(&value);
    redact_key_value_secrets(&value)
}

/// Word characters as regex `\b` sees them, so a prefix inside a word
/// (`tasks_`, `ñsk-`) is not a token start.
fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn is_key_char(character: char) -> bool {
    is_word_char(character) || character == '-'
}

fn is_value_terminator(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            ',' | ';' | '"' | '\'' | '&' | ')' | ']' | '}' | '<' | '>' | '`'
        )
}

/// End (exclusive) of the credential that starts at byte `start`. An
/// existing [`REDACTED`] marker is one whole token, which keeps every pass
/// idempotent (`]` would otherwise end the value inside the marker).
fn token_end(value: &str, start: usize) -> usize {
    if value[start..].starts_with(REDACTED) {
        return start + REDACTED.len();
    }
    value[start..]
        .find(is_value_terminator)
        .map_or(value.len(), |offset| start + offset)
}

/// Index of the first character at or after `start` that is not `skip`.
fn skip_while(value: &str, start: usize, skip: impl Fn(char) -> bool) -> usize {
    value[start..]
        .find(|character: char| !skip(character))
        .map_or(value.len(), |offset| start + offset)
}

/// `scheme://user:password@host` -> `scheme://[redacted]@host`.
fn redact_url_credentials(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(scheme_index) = remaining.find("://") {
        let credential_start = scheme_index + 3;
        let tail = &remaining[credential_start..];
        let boundary = tail
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '/' | '?' | '#')
            })
            .unwrap_or(tail.len());
        let Some(at_index) = tail[..boundary].find('@') else {
            output.push_str(&remaining[..credential_start]);
            remaining = tail;
            continue;
        };
        output.push_str(&remaining[..credential_start]);
        output.push_str(REDACTED);
        output.push('@');
        remaining = &tail[at_index + 1..];
    }
    output.push_str(remaining);
    output
}

/// Tokens recognised by a known prefix, e.g. `ghs_…`, `Bearer …`.
fn redact_secret_prefixes(value: &str) -> String {
    // ASCII lowercasing keeps every byte offset, so indices into `value`
    // stay valid in `lowercase`.
    let lowercase = value.to_ascii_lowercase();
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut previous: Option<char> = None;
    let mut index = 0;
    while let Some(character) = value[index..].chars().next() {
        if previous.is_none_or(|previous| !is_word_char(previous)) {
            let prefix_len = SECRET_PREFIXES
                .iter()
                .filter(|prefix| lowercase[index..].starts_with(*prefix))
                .map(|prefix| prefix.len())
                .max();
            if let Some(prefix_len) = prefix_len {
                let body = index + prefix_len;
                let end = token_end(value, body);
                if end > body {
                    output.push_str(&value[copied..index]);
                    output.push_str(REDACTED);
                    copied = end;
                    index = end;
                    previous = value[..end].chars().next_back();
                    continue;
                }
            }
        }
        previous = Some(character);
        index += character.len_utf8();
    }
    output.push_str(&value[copied..]);
    output
}

/// AWS access key IDs: `AKIA`/`ASIA` followed by 16 uppercase alphanumerics,
/// as a whole word (`\b(AKIA|ASIA)[0-9A-Z]{16}\b`).
fn redact_aws_access_key_ids(value: &str) -> String {
    const LEN: usize = 20;
    let bytes = value.as_bytes();
    let is_word_byte = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut index = 0;
    while index + LEN <= bytes.len() {
        let candidate = &bytes[index..index + LEN];
        let is_key_id = (candidate.starts_with(b"AKIA") || candidate.starts_with(b"ASIA"))
            && candidate[4..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
            && (index == 0 || !is_word_byte(bytes[index - 1]))
            && bytes
                .get(index + LEN)
                .is_none_or(|byte| !is_word_byte(*byte));
        if is_key_id {
            // All 20 bytes are ASCII, so both ends are character boundaries.
            output.push_str(&value[copied..index]);
            output.push_str(REDACTED);
            copied = index + LEN;
            index += LEN;
        } else {
            index += 1;
        }
    }
    output.push_str(&value[copied..]);
    output
}

/// Whether `identifier` is, or ends with a `_`/`-`-separated segment equal
/// to, one of [`SECRET_KEYS`].
fn is_secret_key(identifier: &str) -> bool {
    let normalized = identifier.to_ascii_lowercase().replace('-', "_");
    SECRET_KEYS.iter().any(|key| {
        normalized == *key
            || normalized
                .strip_suffix(key)
                .is_some_and(|rest| rest.ends_with('_'))
    })
}

/// `password=…`, `"token": "…"`, `Authorization: Basic …` and similar.
fn redact_key_value_secrets(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut previous: Option<char> = None;
    let mut index = 0;
    while let Some(character) = value[index..].chars().next() {
        let starts_identifier =
            is_key_char(character) && previous.is_none_or(|previous| !is_key_char(previous));
        if !starts_identifier {
            previous = Some(character);
            index += character.len_utf8();
            continue;
        }

        let identifier_end = skip_while(value, index, is_key_char);
        let identifier = &value[index..identifier_end];
        // Optional closing quote of a JSON key, then optional whitespace.
        let mut separator = identifier_end;
        if value[separator..].starts_with(['"', '\'']) {
            separator += 1;
        }
        separator = skip_while(value, separator, |character| {
            character.is_ascii_whitespace()
        });

        let masked = value[separator..].starts_with(['=', ':']) && is_secret_key(identifier);
        if masked {
            let value_start = skip_while(value, separator + 1, |character| {
                character.is_ascii_whitespace() || matches!(character, '"' | '\'')
            });
            let mut value_end = token_end(value, value_start);
            // `Authorization: Basic <credential>`: the scheme word alone is
            // not the secret; mask the credential after it too.
            let scheme = value[value_start..value_end].to_ascii_lowercase();
            if identifier.to_ascii_lowercase().ends_with("authorization")
                && AUTH_SCHEMES.contains(&scheme.as_str())
            {
                let credential_start = skip_while(value, value_end, |character| {
                    character == ' ' || character == '\t'
                });
                let credential_end = token_end(value, credential_start);
                if credential_start > value_end && credential_end > credential_start {
                    value_end = credential_end;
                }
            }
            if value_end > value_start {
                output.push_str(&value[copied..value_start]);
                output.push_str(REDACTED);
                copied = value_end;
                index = value_end;
                previous = value[..value_end].chars().next_back();
                continue;
            }
        }

        index = identifier_end;
        previous = value[..identifier_end].chars().next_back();
    }
    output.push_str(&value[copied..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inputs from the reported class of leaks, each paired with the secret
    /// that must not survive.
    const LEAKY_INPUTS: &[(&str, &str)] = &[
        (
            "git clone https://x-access-token:ghs_abc@github.com/o/r.git failed",
            "ghs_abc",
        ),
        ("installation token ghs_abc123 rejected", "ghs_abc123"),
        ("pat github_pat_11ABC expired", "github_pat_11ABC"),
        (
            "aws key AKIAIOSFODNN7EXAMPLE denied",
            "AKIAIOSFODNN7EXAMPLE",
        ),
        (
            "session key ASIAIOSFODNN7EXAMPLE denied",
            "ASIAIOSFODNN7EXAMPLE",
        ),
        ("connect failed: password=hunter2", "hunter2"),
        ("Authorization: token ghs_xyz", "ghs_xyz"),
        ("secret_key: abc", "abc"),
        ("Authorization: Basic dXNlcjpwYXNz", "dXNlcjpwYXNz"),
        (
            "Authorization: Bearer opaque-credential-value",
            "opaque-credential-value",
        ),
        ("access_key=AKIDEXAMPLE", "AKIDEXAMPLE"),
        ("private_key: begin-key-material", "begin-key-material"),
        ("client_secret=cs-value", "cs-value"),
        ("credentials: user:pw", "user:pw"),
        ("GITHUB_TOKEN=raw-token-value", "raw-token-value"),
        ("x-api-key: header-key-value", "header-key-value"),
        (r#"{"password": "json-secret"}"#, "json-secret"),
        ("runner glrt-abcdef and deploy gldt-ghijkl", "glrt-abcdef"),
        ("runner glrt-abcdef and deploy gldt-ghijkl", "gldt-ghijkl"),
        ("slack xoxa-1-2 and xoxs-3-4", "xoxa-1-2"),
        ("slack xoxa-1-2 and xoxs-3-4", "xoxs-3-4"),
        ("stripe sk_live_abc and rk_live_def", "sk_live_abc"),
        ("stripe sk_live_abc and rk_live_def", "rk_live_def"),
        ("user tokens ghu_aaa ghr_bbb", "ghu_aaa"),
        ("user tokens ghu_aaa ghr_bbb", "ghr_bbb"),
        // A later, real occurrence of a key must be found even when an
        // earlier mention of the same word is not a `key=value` pair.
        ("password reset failed; password=hunter2", "hunter2"),
    ];

    #[test]
    fn every_recognised_secret_is_masked() {
        for (input, secret) in LEAKY_INPUTS {
            let masked = redact_sensitive_text(input);
            assert!(!masked.contains(secret), "{input:?} -> {masked:?}");
            assert!(masked.contains(REDACTED), "{input:?} -> {masked:?}");
        }
    }

    #[test]
    fn ordinary_text_passes_through_unchanged() {
        for input in [
            "tasks_completed=3 for user 42",
            "Backup of database app_db finished in 12s",
            "risk-free deploy of service ask-me to https://example.com/path",
            "The token expired, sign in again",
            "token_count=3 passwords_checked=2",
            "AKIA123456789012345 is too short and XAKIA1234567890ABCDEF is not a word",
            "Error: connection refused (os error 111)",
            "naïve café — résumé: déjà vu",
            "",
        ] {
            assert_eq!(redact_sensitive_text(input), input, "{input:?}");
        }
    }

    #[test]
    fn masking_is_idempotent() {
        for (input, _) in LEAKY_INPUTS {
            let once = redact_sensitive_text(input);
            assert_eq!(redact_sensitive_text(&once), once, "{input:?}");
        }
    }

    #[test]
    fn surrounding_text_is_preserved() {
        assert_eq!(
            redact_sensitive_text("clone https://x-access-token:ghs_abc@github.com/o/r.git failed"),
            "clone https://[redacted]@github.com/o/r.git failed"
        );
        assert_eq!(
            redact_sensitive_text("Authorization: Basic dXNlcjpwYXNz; retrying"),
            "Authorization: [redacted]; retrying"
        );
        assert_eq!(
            redact_sensitive_text("S3 upload failed (secret_key: abc, region: eu-west-1)"),
            "S3 upload failed (secret_key: [redacted], region: eu-west-1)"
        );
    }

    #[test]
    fn multibyte_text_around_secrets_does_not_panic() {
        let masked = redact_sensitive_text("ñsk-abc ü password=ü€ AKIA1234567890ABCDEFé");
        assert!(!masked.contains("ü€"), "{masked}");
        assert!(!masked.contains("AKIA1234567890ABCDEF"), "{masked}");
        assert!(
            masked.contains("ñsk-abc"),
            "no word boundary before sk-: {masked}"
        );
    }

    #[test]
    fn notification_title_message_and_metadata_are_masked() {
        let mut notification = Notification::new(
            "Clone failed for https://x-access-token:ghs_abc@github.com/o/r.git",
            "error: password=hunter2",
        );
        notification.metadata.insert(
            "detail".to_string(),
            "Authorization: token ghs_xyz".to_string(),
        );
        notification
            .metadata
            .insert("db_password".to_string(), "plain-value".to_string());
        notification
            .metadata
            .insert("environment".to_string(), "production".to_string());

        mask_notification(&mut notification);

        assert!(
            !notification.title.contains("ghs_abc"),
            "{}",
            notification.title
        );
        assert!(
            !notification.message.contains("hunter2"),
            "{}",
            notification.message
        );
        assert_eq!(notification.metadata["detail"], "Authorization: [redacted]");
        assert_eq!(notification.metadata["db_password"], REDACTED);
        assert_eq!(notification.metadata["environment"], "production");
    }
}
