// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Startup state of the console, shared with the proxy.
//!
//! `temps serve` runs the Pingora proxy and the Axum console in one process.
//! The proxy deliberately does not wait for the console: deployed
//! applications stay reachable even when the console cannot start. Without
//! this module, an operator who opens the console URL after a failed start
//! only sees the proxy's generic "Service Unavailable" page and has no way
//! to learn why from the browser.
//!
//! The console's startup task records its outcome here (written once), and
//! the proxy reads it when a request would be forwarded to the console. The
//! read side is lock-free — one atomic load for the phase and one for the
//! write-once failure — so it is safe on the request path.
//!
//! Everything stored here is meant to be shown to an operator, so it is
//! sanitized on the way in: credentials embedded in URLs and `key=value`
//! secrets are redacted and every field is length-bounded. Whether a given
//! client may see the details at all is decided by the proxy.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};

/// Longest error detail kept, in characters. Startup errors carry multi-line
/// remediation text; the useful part is the beginning.
const MAX_DETAIL_CHARS: usize = 2_000;
/// Longest single summary or remediation line kept, in characters.
const MAX_LINE_CHARS: usize = 300;
/// Most remediation steps kept.
const MAX_REMEDIATION_STEPS: usize = 8;

/// The startup check that failed. Stable `code()` values are part of the
/// JSON problem response the proxy serves, so clients can branch on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleStartupCheck {
    /// The Docker daemon could not be reached.
    Docker,
    /// The GeoLite2 database was missing and could not be downloaded.
    GeoDatabase,
    /// The logs directory under the data directory could not be created.
    LogsDirectory,
    /// Log storage environment variables are incomplete or invalid.
    LogStorage,
    /// `TEMPS_ADMIN_EMAIL` / `TEMPS_ADMIN_PASSWORD_FILE` were rejected.
    InitialAdmin,
    /// A plugin failed to initialize.
    PluginInitialization,
    /// The console listener could not bind its address.
    Listener,
    /// Any other startup error.
    Other,
}

impl ConsoleStartupCheck {
    /// Stable machine-readable identifier.
    pub fn code(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::GeoDatabase => "geo_database",
            Self::LogsDirectory => "logs_directory",
            Self::LogStorage => "log_storage",
            Self::InitialAdmin => "initial_admin",
            Self::PluginInitialization => "plugin_initialization",
            Self::Listener => "listener",
            Self::Other => "other",
        }
    }

    /// Human-readable name of the check.
    pub fn label(self) -> &'static str {
        match self {
            Self::Docker => "Docker",
            Self::GeoDatabase => "GeoLite2 database",
            Self::LogsDirectory => "Logs directory",
            Self::LogStorage => "Log storage configuration",
            Self::InitialAdmin => "Initial admin account",
            Self::PluginInitialization => "Plugin initialization",
            Self::Listener => "Console listener",
            Self::Other => "Console startup",
        }
    }
}

/// Why the console failed to start, in operator-facing terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleStartupFailure {
    /// Which check failed.
    pub check: ConsoleStartupCheck,
    /// One line naming the problem, e.g. "Docker is not reachable".
    pub summary: String,
    /// The underlying error, redacted and bounded.
    pub detail: String,
    /// Concrete steps that fix the problem, in order.
    pub remediation: Vec<String>,
    /// When the failure was recorded.
    pub failed_at: DateTime<Utc>,
}

impl ConsoleStartupFailure {
    /// Build a failure record. Every text field is redacted and bounded
    /// here, so callers can pass raw error text.
    pub fn new(
        check: ConsoleStartupCheck,
        summary: impl AsRef<str>,
        detail: impl AsRef<str>,
        remediation: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            check,
            summary: bound(&redact_secrets(summary.as_ref()), MAX_LINE_CHARS),
            detail: bound(&redact_secrets(detail.as_ref().trim()), MAX_DETAIL_CHARS),
            remediation: remediation
                .into_iter()
                .map(|step| bound(&redact_secrets(step.trim()), MAX_LINE_CHARS))
                .filter(|step| !step.is_empty())
                .take(MAX_REMEDIATION_STEPS)
                .collect(),
            failed_at: Utc::now(),
        }
    }
}

/// Lifecycle phase of the console within this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolePhase {
    /// Startup is still running (dependency checks, plugin init, ...).
    Starting,
    /// The console listener is serving requests.
    Running,
    /// Startup failed; see [`ConsoleStartupState::failure`].
    Failed,
}

const PHASE_STARTING: u8 = 0;
const PHASE_RUNNING: u8 = 1;
const PHASE_FAILED: u8 = 2;

/// Shared, write-once record of the console's startup outcome.
///
/// The console task is the only writer; the proxy is the reader. Reads are
/// an acquire atomic load plus a `OnceLock::get`, never a lock.
#[derive(Debug)]
pub struct ConsoleStartupState {
    phase: AtomicU8,
    failure: OnceLock<ConsoleStartupFailure>,
}

impl Default for ConsoleStartupState {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleStartupState {
    pub fn new() -> Self {
        Self {
            phase: AtomicU8::new(PHASE_STARTING),
            failure: OnceLock::new(),
        }
    }

    /// Current phase.
    pub fn phase(&self) -> ConsolePhase {
        match self.phase.load(Ordering::Acquire) {
            PHASE_RUNNING => ConsolePhase::Running,
            PHASE_FAILED => ConsolePhase::Failed,
            _ => ConsolePhase::Starting,
        }
    }

    /// The console listener is serving. Ignored after a failure: a failed
    /// start is final for the life of the process.
    pub fn mark_running(&self) {
        let _ = self.phase.compare_exchange(
            PHASE_STARTING,
            PHASE_RUNNING,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Record why startup failed. Only the first failure is kept; returns
    /// `false` when one was already recorded.
    pub fn record_failure(&self, failure: ConsoleStartupFailure) -> bool {
        let stored = self.failure.set(failure).is_ok();
        // Publish the failure before the phase so a reader that observes
        // `Failed` always finds it.
        self.phase.store(PHASE_FAILED, Ordering::Release);
        stored
    }

    /// Why startup failed, if it did.
    pub fn failure(&self) -> Option<&ConsoleStartupFailure> {
        self.failure.get()
    }
}

/// Redact credentials from free-form error text: the userinfo part of any
/// `scheme://user:password@host` URL, and the value of `password=`,
/// `token=`, `secret=` and similar `key=value` / `key: value` pairs.
pub fn redact_secrets(text: &str) -> String {
    redact_key_values(&redact_url_userinfo(text))
}

fn redact_url_userinfo(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(scheme_end) = remaining.find("://") {
        let authority_start = scheme_end + 3;
        output.push_str(&remaining[..authority_start]);
        let tail = &remaining[authority_start..];
        let authority_end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\''))
            .unwrap_or(tail.len());
        match tail[..authority_end].rfind('@') {
            Some(at) => {
                output.push_str("***@");
                remaining = &tail[at + 1..];
            }
            None => remaining = tail,
        }
    }
    output.push_str(remaining);
    output
}

const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_key",
    "license_key",
    "access_token",
    "api_token",
    "secret_access_key",
    "authorization",
    "cookie",
    "session",
];

fn redact_key_values(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    let mut cursor = 0;
    while cursor < text.len() {
        let Some((key_start, key)) = SECRET_KEYS
            .iter()
            .filter_map(|key| lower[cursor..].find(key).map(|i| (cursor + i, *key)))
            .min_by_key(|(start, key)| (*start, std::cmp::Reverse(key.len())))
        else {
            break;
        };
        let key_end = key_start + key.len();
        // Must be a whole word, not e.g. "tokenizer".
        let starts_word = key_start == 0 || !is_word_byte(bytes[key_start - 1]);
        let mut value_start = key_end;
        // JSON/debug output may quote the key as well as the value.
        if key_start > 0
            && matches!(bytes[key_start - 1], b'\'' | b'"')
            && bytes.get(value_start) == Some(&bytes[key_start - 1])
        {
            value_start += 1;
        }
        while value_start < text.len() && bytes[value_start].is_ascii_whitespace() {
            value_start += 1;
        }
        let has_separator = value_start < text.len() && matches!(bytes[value_start], b'=' | b':');
        if !starts_word || !has_separator {
            cursor = key_end;
            continue;
        }
        value_start += 1;
        while value_start < text.len() && bytes[value_start].is_ascii_whitespace() {
            value_start += 1;
        }
        let quoted = bytes
            .get(value_start)
            .copied()
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        let value_end = if let Some(quote) = quoted {
            value_start += 1;
            let mut end = value_start;
            while end < bytes.len() {
                if bytes[end] == b'\\' {
                    end = (end + 2).min(bytes.len());
                } else if bytes[end] == quote {
                    break;
                } else {
                    end += 1;
                }
            }
            end
        } else {
            if matches!(key, "authorization" | "cookie") {
                // Header credentials can contain spaces, commas and multiple
                // cookie values. Redact the whole value, up to the next line.
                text[value_start..]
                    .find(['\r', '\n'])
                    .map_or(text.len(), |i| value_start + i)
            } else {
                text[value_start..]
                    .find(|c: char| c.is_whitespace() || matches!(c, '&' | ',' | ';' | '"' | '\''))
                    .map_or(text.len(), |i| value_start + i)
            }
        };
        if value_end > value_start {
            output.push_str(&text[copied..value_start]);
            output.push_str("***");
            copied = value_end;
        }
        cursor = value_end.max(key_end);
    }
    output.push_str(&text[copied..]);
    output
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Truncate to `max_chars` characters, marking the cut with an ellipsis.
fn bound(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut bounded: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    bounded.push('…');
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_is_starting_without_failure() {
        let state = ConsoleStartupState::new();
        assert_eq!(state.phase(), ConsolePhase::Starting);
        assert!(state.failure().is_none());
    }

    #[test]
    fn mark_running_moves_starting_to_running() {
        let state = ConsoleStartupState::new();
        state.mark_running();
        assert_eq!(state.phase(), ConsolePhase::Running);
    }

    #[test]
    fn failure_is_write_once_and_final() {
        let state = ConsoleStartupState::new();
        let first = ConsoleStartupFailure::new(
            ConsoleStartupCheck::Docker,
            "Docker is not reachable",
            "Socket not found: /var/run/docker.sock",
            vec!["Start the Docker daemon".to_string()],
        );
        let second = ConsoleStartupFailure::new(
            ConsoleStartupCheck::Other,
            "something else",
            "later error",
            Vec::new(),
        );
        assert!(state.record_failure(first));
        assert!(!state.record_failure(second));
        state.mark_running();

        assert_eq!(state.phase(), ConsolePhase::Failed);
        let failure = state.failure().expect("failure recorded");
        assert_eq!(failure.check, ConsoleStartupCheck::Docker);
        assert_eq!(failure.detail, "Socket not found: /var/run/docker.sock");
    }

    #[test]
    fn redacts_url_credentials() {
        let text = "connect to postgres://temps:hunter2@db.internal:5432/temps failed";
        assert_eq!(
            redact_secrets(text),
            "connect to postgres://***@db.internal:5432/temps failed"
        );
    }

    #[test]
    fn leaves_urls_without_credentials_alone() {
        let text = "Socket not found: unix:///var/run/docker.sock and https://temps.sh/docs";
        assert_eq!(redact_secrets(text), text);
    }

    #[test]
    fn redacts_key_value_secrets() {
        assert_eq!(
            redact_secrets("auth failed: password=hunter2 user=temps"),
            "auth failed: password=*** user=temps"
        );
        assert_eq!(
            redact_secrets("Token: abc123def, retrying"),
            "Token: ***, retrying"
        );
        assert_eq!(
            redact_secrets("license_key = ABCD-1234"),
            "license_key = ***"
        );
    }

    #[test]
    fn redacts_quoted_and_header_secrets() {
        assert_eq!(
            redact_secrets(r#"{"token": "private value", "password": "secret"}"#),
            r#"{"token": "***", "password": "***"}"#
        );
        assert_eq!(
            redact_secrets("password='two words' user=temps"),
            "password='***' user=temps"
        );
        assert_eq!(
            redact_secrets("Authorization: Bearer private-token\nretrying"),
            "Authorization: ***\nretrying"
        );
        assert_eq!(
            redact_secrets(
                "Authorization: Bearer   private-token\nCookie: auth_session=hidden; token=secret"
            ),
            "Authorization: ***\nCookie: ***"
        );
        assert_eq!(
            redact_secrets("access_token=private; secret_access_key=hidden"),
            "access_token=***; secret_access_key=***"
        );
        assert_eq!(
            redact_secrets(r#"token="escaped \" secret" retrying"#),
            r#"token="***" retrying"#
        );
    }

    #[test]
    fn does_not_redact_words_that_merely_contain_a_key() {
        let text = "tokenizer: ready; the password file does not meet requirements";
        assert_eq!(redact_secrets(text), text);
    }

    #[test]
    fn failure_fields_are_redacted_and_bounded() {
        let long = "x".repeat(MAX_DETAIL_CHARS + 50);
        let failure = ConsoleStartupFailure::new(
            ConsoleStartupCheck::Other,
            "postgres://u:p@h/db",
            &long,
            (0..20).map(|i| format!("step {i} token=abc")),
        );
        assert_eq!(failure.summary, "postgres://***@h/db");
        assert_eq!(failure.detail.chars().count(), MAX_DETAIL_CHARS);
        assert!(failure.detail.ends_with('…'));
        assert_eq!(failure.remediation.len(), MAX_REMEDIATION_STEPS);
        assert_eq!(failure.remediation[0], "step 0 token=***");
    }

    #[test]
    fn check_codes_are_stable() {
        assert_eq!(ConsoleStartupCheck::Docker.code(), "docker");
        assert_eq!(ConsoleStartupCheck::InitialAdmin.code(), "initial_admin");
        assert_eq!(ConsoleStartupCheck::Other.code(), "other");
    }
}
