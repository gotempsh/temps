// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Provider-neutral contract for signing an AI CLI in to a subscription from
//! the Temps console.
//!
//! The CLI owns the whole exchange and its credential store. Temps only
//! relays what the operator must see (a URL, optionally a device code) and
//! what the operator types back (a pasted code). Nothing the CLI prints is
//! logged or persisted, and no token file is read while the flow runs.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

/// Hard limit on one sign-in from start to credential import.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Maximum sign-ins running at once on this server.
pub const MAX_CONCURRENT_LOGINS: usize = 4;

/// How long a CLI may take to produce its sign-in link or device code.
pub const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(60);

/// Largest pasted authorization code accepted.
pub const MAX_CODE_BYTES: usize = 2048;

/// Upper bound on bytes scanned from CLI output looking for the sign-in link.
pub const MAX_SCANNED_OUTPUT_BYTES: usize = 256 * 1024;

/// Hosts a Claude sign-in link may point at. Exact match only: a link that is
/// shown to an operator and opened in their browser must not be steerable by
/// whatever the CLI happens to print.
pub const CLAUDE_LOGIN_HOSTS: &[&str] = &[
    "claude.ai",
    "claude.com",
    "auth.anthropic.com",
    "console.anthropic.com",
    "platform.claude.com",
];

/// Hosts a Codex device-code verification page may live on.
pub const CODEX_LOGIN_HOSTS: &[&str] = &["auth.openai.com", "chatgpt.com", "openai.com"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoginPhase {
    /// The CLI is starting and has not produced a link yet.
    Starting,
    /// The operator must act: open the link and, for Claude, paste the code.
    Waiting,
    /// A code was handed to the CLI, or the CLI finished, and Temps is
    /// confirming the sign-in.
    Verifying,
    Succeeded,
    Failed,
    Cancelled,
}

impl LoginPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Waiting => "waiting",
            Self::Verifying => "verifying",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// What the operator needs to continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginChallenge {
    /// Page the operator opens in their own browser.
    pub verification_url: String,
    /// Device code to enter on that page, for device-code flows.
    pub user_code: Option<String>,
    /// True when the operator must paste a code back (Claude); false when the
    /// CLI completes by itself after the browser step (Codex device code).
    pub requires_code: bool,
}

/// Operations available while a sign-in is waiting on the operator.
#[async_trait]
pub trait LoginControl: Send + Sync {
    /// Hand the operator's pasted code to the CLI.
    async fn submit_code(&self, code: &str) -> Result<(), LoginError>;
    /// Ask the CLI process to stop. Idempotent.
    fn cancel(&self);
}

pub type LoginCompletion = Pin<Box<dyn Future<Output = Result<(), LoginError>> + Send>>;

/// A started sign-in. Dropping `completion` kills the CLI process.
pub struct LoginHandle {
    pub challenge: LoginChallenge,
    pub control: Arc<dyn LoginControl>,
    /// Resolves when the CLI exits (Claude) or reports completion (Codex).
    pub completion: LoginCompletion,
}

#[derive(Debug, Error)]
pub enum LoginError {
    #[error("Provider '{provider_id}' does not support signing in from the console")]
    NotSupported { provider_id: String },

    #[error("Unknown AI provider '{provider_id}'")]
    UnknownProvider { provider_id: String },

    #[error("The '{provider_id}' CLI is not installed on this server")]
    NotInstalled { provider_id: String },

    #[error("Could not start the '{provider_id}' CLI: {reason}")]
    Spawn { provider_id: String, reason: String },

    #[error("The '{provider_id}' CLI did not offer a sign-in link within {waited_secs}s")]
    NoChallenge {
        provider_id: String,
        waited_secs: u64,
    },

    #[error(
        "The '{provider_id}' CLI offered a sign-in link on host '{host}', which is not on the \
         allowed list for this provider"
    )]
    DisallowedHost { provider_id: String, host: String },

    #[error("The '{provider_id}' sign-in link is not a valid https URL: {reason}")]
    InvalidUrl { provider_id: String, reason: String },

    #[error("The pasted code is not valid: {reason}")]
    InvalidCode { reason: String },

    #[error("Provider '{provider_id}' is not waiting for a code (current phase: {phase})")]
    NotWaitingForCode { provider_id: String, phase: String },

    #[error("Sign-in for '{provider_id}' did not complete: the CLI rejected the code or exited with status {exit_code}")]
    CliRejected { provider_id: String, exit_code: i32 },

    #[error("The '{provider_id}' sign-in did not complete: it was declined, expired, or the CLI stopped before finishing")]
    NotCompleted { provider_id: String },

    #[error("Sign-in for '{provider_id}' timed out after {timeout_secs}s")]
    Timeout {
        provider_id: String,
        timeout_secs: u64,
    },

    #[error("Sign-in for '{provider_id}' was cancelled")]
    Cancelled { provider_id: String },

    #[error("A sign-in for '{provider_id}' is already in progress")]
    AlreadyInProgress { provider_id: String },

    #[error("Too many sign-ins are running ({limit} at once). Wait for one to finish and retry")]
    ConcurrencyLimit { limit: usize },

    #[error("No sign-in has been started for '{provider_id}'")]
    NoSession { provider_id: String },

    #[error(
        "The '{provider_id}' CLI reports it is still not signed in after the flow finished \
         (installed: {installed})"
    )]
    NotAuthenticated {
        provider_id: String,
        installed: bool,
    },

    #[error(
        "The CLI for '{provider_id}' finished signing in but left no credential file at the \
         expected location; this host may keep CLI credentials in a system keychain, which \
         Temps cannot import"
    )]
    CredentialFileMissing { provider_id: String },

    #[error("The credential produced by '{provider_id}' could not be imported: {reason}")]
    CredentialImport { provider_id: String, reason: String },

    #[error(
        "The Temps data directory for '{provider_id}' is not writable by the user the CLI runs \
         as ('{run_user}')"
    )]
    StateDirectoryInaccessible {
        provider_id: String,
        run_user: String,
    },

    #[error("I/O error during '{provider_id}' sign-in while {operation}: {reason}")]
    Io {
        provider_id: String,
        operation: String,
        reason: String,
    },
}

/// Validate a sign-in link printed by a CLI before it is shown to anyone.
///
/// Requires `https`, no embedded credentials, the default port and a host on
/// `allowed_hosts` (exact, case-insensitive). The error names only the host,
/// never the full link, which carries one-time `state` material.
pub fn validate_verification_url(
    provider_id: &str,
    raw: &str,
    allowed_hosts: &[&str],
) -> Result<String, LoginError> {
    let invalid = |reason: &str| LoginError::InvalidUrl {
        provider_id: provider_id.to_string(),
        reason: reason.to_string(),
    };
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 4096 {
        return Err(invalid("empty or longer than 4096 bytes"));
    }
    let url = reqwest::Url::parse(raw).map_err(|_| invalid("not a parseable URL"))?;
    if url.scheme() != "https" {
        return Err(invalid("scheme is not https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("contains embedded credentials"));
    }
    if url.port().is_some() {
        return Err(invalid("uses a non-default port"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("has no host"))?
        .to_ascii_lowercase();
    if !allowed_hosts.iter().any(|allowed| *allowed == host) {
        return Err(LoginError::DisallowedHost {
            provider_id: provider_id.to_string(),
            host,
        });
    }
    Ok(url.to_string())
}

/// Validate a pasted authorization code before it reaches a CLI's stdin.
///
/// Codes are short opaque strings. Anything containing whitespace or control
/// characters is refused so a pasted value can never become a second line (a
/// second answer to the CLI's prompt).
pub fn validate_pasted_code(raw: &str) -> Result<String, LoginError> {
    let code = raw.trim();
    if code.is_empty() {
        return Err(LoginError::InvalidCode {
            reason: "the code is empty".to_string(),
        });
    }
    if code.len() > MAX_CODE_BYTES {
        return Err(LoginError::InvalidCode {
            reason: format!("the code is longer than {MAX_CODE_BYTES} bytes"),
        });
    }
    if !code.chars().all(|c| c.is_ascii_graphic()) {
        return Err(LoginError::InvalidCode {
            reason: "the code may only contain visible ASCII characters".to_string(),
        });
    }
    Ok(code.to_string())
}

/// What scanning one line of CLI output found.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkScan {
    /// An allow-listed sign-in link.
    Accepted(String),
    /// A link on a host that is not allow-listed and looks like a sign-in
    /// page. Fail fast instead of waiting out the challenge timeout.
    Rejected { host: String },
    /// No link, or an unrelated allow-list miss (documentation link).
    Nothing,
}

/// Look for the sign-in link in one line of CLI output.
pub fn scan_line_for_link(provider_id: &str, line: &str, allowed_hosts: &[&str]) -> LinkScan {
    let mut rest = line;
    while let Some(start) = rest.find("https://") {
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(candidate.len());
        let link = candidate[..end].trim_end_matches(['.', ',', ')', ']']);
        match validate_verification_url(provider_id, link, allowed_hosts) {
            Ok(url) => return LinkScan::Accepted(url),
            Err(LoginError::DisallowedHost { host, .. }) => {
                let lower = link.to_ascii_lowercase();
                if lower.contains("oauth") || lower.contains("authorize") || lower.contains("login")
                {
                    return LinkScan::Rejected { host };
                }
            }
            Err(_) => {}
        }
        rest = &candidate[end.max(1)..];
    }
    LinkScan::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_allow_listed_https_links() {
        for host in CLAUDE_LOGIN_HOSTS {
            let url = format!("https://{host}/oauth/authorize?code=true&state=abc");
            assert!(
                validate_verification_url("claude_cli", &url, CLAUDE_LOGIN_HOSTS).is_ok(),
                "{host}"
            );
        }
    }

    #[test]
    fn rejects_hosts_outside_the_allow_list() {
        let err = validate_verification_url(
            "claude_cli",
            "https://evil.example/oauth/authorize",
            CLAUDE_LOGIN_HOSTS,
        )
        .expect_err("must be refused");
        assert!(matches!(
            err,
            LoginError::DisallowedHost { ref host, .. } if host == "evil.example"
        ));
    }

    #[test]
    fn rejects_lookalike_and_userinfo_hosts() {
        for url in [
            "https://claude.ai.evil.example/x",
            "https://evilclaude.ai/x",
            "https://claude.ai@evil.example/x",
            "https://evil.example\\@claude.ai/x",
            "https://claude.ai:8443/x",
            "http://claude.ai/x",
            "ftp://claude.ai/x",
            "javascript:alert(1)",
            "",
        ] {
            assert!(
                validate_verification_url("claude_cli", url, CLAUDE_LOGIN_HOSTS).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn disallowed_host_error_never_echoes_the_query() {
        let err = validate_verification_url(
            "claude_cli",
            "https://evil.example/oauth?state=SECRET-STATE",
            CLAUDE_LOGIN_HOSTS,
        )
        .expect_err("refused");
        assert!(!err.to_string().contains("SECRET-STATE"));
    }

    #[test]
    fn host_match_is_case_insensitive() {
        assert!(validate_verification_url(
            "claude_cli",
            "https://CLAUDE.AI/oauth/authorize",
            CLAUDE_LOGIN_HOSTS
        )
        .is_ok());
    }

    #[test]
    fn pasted_code_rejects_whitespace_and_control_characters() {
        assert_eq!(
            validate_pasted_code("  abc123#def456  ").expect("valid"),
            "abc123#def456"
        );
        for bad in [
            "",
            "   ",
            "a b",
            "abc\ndef",
            "abc\rdef",
            "abc\u{0}",
            "caf\u{e9}",
        ] {
            assert!(validate_pasted_code(bad).is_err(), "{bad:?}");
        }
        assert!(validate_pasted_code(&"a".repeat(MAX_CODE_BYTES + 1)).is_err());
    }

    #[test]
    fn line_scan_extracts_the_first_allowed_link() {
        let line = "If the browser didn't open, visit: https://claude.ai/oauth/authorize?code=true&state=s1.";
        match scan_line_for_link("claude_cli", line, CLAUDE_LOGIN_HOSTS) {
            LinkScan::Accepted(url) => {
                assert!(url.starts_with("https://claude.ai/oauth/authorize"));
                assert!(!url.ends_with('.'));
            }
            other => panic!("unexpected scan result: {other:?}"),
        }
    }

    #[test]
    fn line_scan_ignores_unrelated_links_but_flags_foreign_sign_in_pages() {
        assert_eq!(
            scan_line_for_link(
                "claude_cli",
                "docs: https://docs.example.org/guide",
                CLAUDE_LOGIN_HOSTS
            ),
            LinkScan::Nothing
        );
        assert_eq!(
            scan_line_for_link(
                "claude_cli",
                "visit https://evil.example/oauth/authorize?x=1",
                CLAUDE_LOGIN_HOSTS
            ),
            LinkScan::Rejected {
                host: "evil.example".to_string()
            }
        );
        assert_eq!(
            scan_line_for_link("claude_cli", "no link here", CLAUDE_LOGIN_HOSTS),
            LinkScan::Nothing
        );
    }

    #[test]
    fn phases_report_terminal_state() {
        assert!(!LoginPhase::Starting.is_terminal());
        assert!(!LoginPhase::Waiting.is_terminal());
        assert!(!LoginPhase::Verifying.is_terminal());
        assert!(LoginPhase::Succeeded.is_terminal());
        assert!(LoginPhase::Failed.is_terminal());
        assert!(LoginPhase::Cancelled.is_terminal());
    }
}
