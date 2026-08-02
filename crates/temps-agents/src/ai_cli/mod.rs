pub mod catalog;
pub mod claude;
pub mod codex;
pub mod opencode;

pub use catalog::{
    find_provider, AuthFlavor, CredentialFormat, ProviderCatalogEntry, PROVIDER_CATALOG,
};

use async_trait::async_trait;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::error::AgentError;

/// Callback invoked for each line of AI CLI output (for real-time streaming)
pub type OnEventCallback =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

pub struct AiRunConfig {
    pub work_dir: PathBuf,
    pub prompt: String,
    pub api_key: String,
    pub max_turns: i32,
    pub timeout: Duration,
    /// Optional preferred model name (e.g. "sonnet", "gpt-5-codex").
    /// `None` lets the CLI pick its default.
    pub model: Option<String>,
    /// Optional callback for streaming each line of output in real-time
    pub on_event: Option<OnEventCallback>,
}

pub struct AiRunResult {
    pub output: String,
    pub exit_code: i32,
    pub tokens_input: Option<i32>,
    pub tokens_output: Option<i32>,
    pub model: Option<String>,
    /// If the provider knows which files it changed, list them here.
    /// If `None`, the executor will detect changes via `git diff`.
    pub changed_files: Option<Vec<String>>,
    /// Claude CLI session ID (UUID) extracted from the `system/init` event.
    /// Used to resume the conversation in a workspace via `--resume`.
    pub session_id: Option<String>,
    /// True when the CLI hit the max turns limit without completing.
    pub is_max_turns_error: bool,
}

/// Status of the AI CLI tool on this server.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiCliStatus {
    pub provider: String,
    pub installed: bool,
    pub version: Option<String>,
    pub authenticated: bool,
    pub auth_method: Option<String>,
    pub email: Option<String>,
    pub subscription_type: Option<String>,
    /// Instructions for the user if not installed or not authenticated.
    pub setup_hint: Option<String>,
}

#[async_trait]
pub trait AiCliProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn check_installed(&self) -> bool;
    async fn get_status(&self) -> AiCliStatus;
    async fn run(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError>;
    /// Continue an existing conversation in the same work directory.
    /// Uses `--continue` to resume the most recent session.
    async fn continue_conversation(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError>;
}

/// Parse raw CLI output into the common parsed shape for any provider.
/// Codex/OpenCode don't report session ids or max-turn errors, so those
/// fields come back as `None`/`false` for them.
pub fn parse_output(provider: &str, output: &str) -> claude::ParsedClaudeOutput {
    match provider {
        "codex_cli" => {
            let (tokens_input, tokens_output, model) = codex::parse_codex_output(output);
            claude::ParsedClaudeOutput {
                tokens_input,
                tokens_output,
                model,
                session_id: None,
                is_max_turns_error: false,
            }
        }
        "opencode" => {
            let (tokens_input, tokens_output, model) = opencode::parse_opencode_output(output);
            claude::ParsedClaudeOutput {
                tokens_input,
                tokens_output,
                model,
                session_id: None,
                is_max_turns_error: false,
            }
        }
        _ => claude::parse_claude_output(output),
    }
}

/// Turn a failed CLI's raw stream output into a short, actionable message.
/// The full output is preserved in the run's `ai_event` logs — this summary
/// is what lands in `error_message` and the failure banner, so a self-hosted
/// user sees "out of credits, retry with another provider" instead of a
/// multi-kilobyte JSON dump.
pub fn summarize_cli_failure(provider: &str, output: &str) -> String {
    // Claude rate-limit frame: {"type":"rate_limit_event","rate_limit_info":
    // {"status":...,"overageStatus":"rejected","resetsAt":...}}
    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("rate_limit_event") {
            let info = v.get("rate_limit_info");
            let rejected = info
                .and_then(|i| i.get("overageStatus"))
                .and_then(|s| s.as_str())
                == Some("rejected");
            let exhausted =
                info.and_then(|i| i.get("status")).and_then(|s| s.as_str()) == Some("exceeded");
            if rejected || exhausted {
                return format!(
                    "The {} subscription hit its usage limit (out of credits). \
                     Wait for the limit to reset, or start a new run with a \
                     different provider or an API key.",
                    provider
                );
            }
        }
        // Claude terminal error frame: {"type":"result","is_error":true,"result":"..."}
        if v.get("type").and_then(|t| t.as_str()) == Some("result")
            && v.get("is_error").and_then(|b| b.as_bool()) == Some(true)
        {
            if let Some(msg) = v.get("result").and_then(|r| r.as_str()) {
                if !msg.trim().is_empty() {
                    return scrub_and_bound(msg);
                }
            }
        }
    }
    // Fallback: last non-JSON line (CLIs print human errors to the tail),
    // else a bounded slice of the output.
    if let Some(tail) = output
        .lines()
        .map(str::trim)
        .rev()
        .find(|l| !l.is_empty() && !l.starts_with('{'))
    {
        return scrub_and_bound(tail);
    }
    let bounded = scrub_and_bound(output.trim());
    if bounded.is_empty() {
        format!("{} exited with an error but produced no output", provider)
    } else {
        bounded
    }
}

/// Bound an error snippet to 500 chars and redact anything that looks like a
/// credential — CLI errors occasionally echo key fragments, and this string
/// lands in `error_message`, which is returned to any user with read access.
pub fn scrub_and_bound(s: &str) -> String {
    let bounded: String = s.trim().chars().take(500).collect();
    // Redact common secret prefixes followed by their token body.
    let mut out = String::with_capacity(bounded.len());
    let mut rest = bounded.as_str();
    const PREFIXES: &[&str] = &["sk-ant-", "sk-", "Bearer ", "ghp_", "gho_", "glpat-"];
    while !rest.is_empty() {
        // Redact the EARLIEST secret in `rest` across all prefixes, not the
        // first prefix that matches anywhere — otherwise a token whose prefix
        // sits later in the list but earlier in the string would be emitted
        // verbatim before the "matched" one.
        let earliest = PREFIXES
            .iter()
            .filter_map(|p| rest.find(p).map(|idx| (idx, p.len())))
            .min_by_key(|(idx, _)| *idx);
        match earliest {
            Some((idx, plen)) => {
                out.push_str(&rest[..idx]);
                out.push_str("[redacted]");
                // Skip the prefix and the following token body (non-space run).
                let after = &rest[idx + plen..];
                let body_end = after
                    .find(|c: char| c.is_whitespace())
                    .unwrap_or(after.len());
                rest = &after[body_end..];
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// Create an AI CLI provider by name
pub fn create_provider(name: &str) -> Option<Box<dyn AiCliProvider>> {
    match name {
        "claude_cli" => Some(Box::new(claude::ClaudeCliProvider)),
        "codex_cli" => Some(Box::new(codex::CodexCliProvider)),
        "opencode" => Some(Box::new(opencode::OpenCodeCliProvider)),
        _ => None,
    }
}

/// All supported provider names.
pub const PROVIDER_NAMES: &[(&str, &str)] = &[
    ("claude_cli", "Claude Code"),
    ("opencode", "OpenCode"),
    ("codex_cli", "Codex"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_cli_failure_rate_limit_rejected() {
        let output = r#"{"type":"system","subtype":"init","session_id":"abc"}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1784910000,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"out_of_credits","isUsingOverage":false}}
{"type":"assistant","message":{}}"#;
        let msg = summarize_cli_failure("claude_cli", output);
        assert!(msg.contains("usage limit"), "got: {}", msg);
        assert!(msg.contains("different provider"), "got: {}", msg);
    }

    #[test]
    fn test_summarize_cli_failure_result_error_frame() {
        let output = r#"{"type":"result","is_error":true,"result":"Invalid API key"}"#;
        assert_eq!(
            summarize_cli_failure("claude_cli", output),
            "Invalid API key"
        );
    }

    #[test]
    fn test_summarize_cli_failure_falls_back_to_tail_line() {
        let output = "{\"type\":\"noise\"}\nError: not logged in\n";
        assert_eq!(
            summarize_cli_failure("codex_cli", output),
            "Error: not logged in"
        );
    }

    #[test]
    fn test_summarize_cli_failure_empty_output() {
        let msg = summarize_cli_failure("claude_cli", "");
        assert!(msg.contains("no output"));
    }

    #[test]
    fn test_scrub_and_bound_redacts_secrets() {
        assert_eq!(
            scrub_and_bound("auth failed: sk-ant-abc123XYZ is invalid"),
            "auth failed: [redacted] is invalid"
        );
        assert_eq!(
            scrub_and_bound("header Bearer eyJhbGciOi.foo rejected"),
            "header [redacted] rejected"
        );
        // Non-secret text is left intact.
        assert_eq!(
            scrub_and_bound("plain error message"),
            "plain error message"
        );
    }

    #[test]
    fn test_scrub_and_bound_redacts_earliest_secret_first() {
        // `Bearer ` appears before `sk-ant-` in the string but later in the
        // prefix list — the earlier one must still be redacted, not emitted
        // verbatim before the "matched" one.
        let out = scrub_and_bound("Bearer abc123 then sk-ant-xyz789 end");
        assert!(!out.contains("abc123"), "leaked Bearer token: {}", out);
        assert!(!out.contains("xyz789"), "leaked sk-ant token: {}", out);
        assert_eq!(out, "[redacted] then [redacted] end");
    }

    #[test]
    fn test_scrub_and_bound_caps_length() {
        let long = "x".repeat(1000);
        assert_eq!(scrub_and_bound(&long).len(), 500);
    }

    #[test]
    fn test_summarize_cli_failure_result_frame_is_scrubbed_and_bounded() {
        let secret = "sk-ant-topsecretkey";
        let output = format!(
            r#"{{"type":"result","is_error":true,"result":"key {} rejected"}}"#,
            secret
        );
        let msg = summarize_cli_failure("claude_cli", &output);
        assert!(!msg.contains(secret), "secret leaked: {}", msg);
        assert!(msg.contains("[redacted]"));
    }
}
