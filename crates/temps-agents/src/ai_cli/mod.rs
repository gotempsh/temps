pub mod catalog;
pub mod claude;
pub mod codex;
pub mod opencode;

pub use catalog::{
    find_provider, AuthFlavor, CredentialFormat, HostAccessRequirement, ProviderCatalogEntry,
    ProviderOption, PROVIDER_CATALOG,
};

use async_trait::async_trait;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use temps_ai::streaming::{PermissionDecision, PermissionRequest};
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::error::AgentError;

/// Remove the Temps server environment before launching an AI harness.
///
/// Harnesses may expose native shell tools to the model, so inheriting the
/// server process environment would also expose database URLs, signing keys,
/// and credentials for unrelated integrations. Keep only the small set needed
/// to locate the executable and the authenticated user's CLI config. Provider
/// credentials and the ephemeral MCP token are added explicitly by adapters.
pub(crate) fn sanitize_command_environment(command: &mut Command) {
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TERM",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ];
    let preserved = ALLOWED
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    command.env_clear();
    command.envs(preserved);
}

pub(crate) fn copy_environment_variable(command: &mut Command, name: &str) {
    if let Some(value) = std::env::var_os(name) {
        command.env(name, value);
    }
}

/// Callback invoked for each line of AI CLI output (for real-time streaming)
pub type OnEventCallback =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Bridge for the `--permission-prompt-tool stdio` interactive control protocol
/// (ADR-038 Phase 2, milestone 3+).  Wired by `ConversationService` so that
/// `run_interactive` can register pending permissions in the in-process registry
/// and await human decisions without knowing about the service layer.
///
/// `on_permission_request` is called once per `control_request` frame.  It MUST:
/// 1. Insert a `oneshot::Sender<PermissionDecision>` keyed by `req.id` into the
///    shared pending-permission registry (so the resolve endpoint can claim it).
/// 2. Emit an appropriate SSE event (e.g. `ChatStreamEvent::PermissionRequested`)
///    so the UI can render the approval card.
/// 3. Return the matching `oneshot::Receiver` so `run_interactive` can `await`
///    the decision and write the `control_response` back to the CLI's stdin.
pub struct PermissionBridge {
    pub on_permission_request:
        Arc<dyn Fn(PermissionRequest) -> oneshot::Receiver<PermissionDecision> + Send + Sync>,
}

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub url: String,
    /// Ephemeral bearer value valid only for this CLI turn.
    pub authorization_token: String,
}

pub struct AiRunConfig {
    pub work_dir: PathBuf,
    pub prompt: String,
    pub api_key: String,
    pub max_turns: i32,
    pub timeout: Duration,
    /// Optional preferred model name (e.g. "sonnet", "gpt-5-codex").
    /// `None` lets the CLI pick its default.
    pub model: Option<String>,
    /// Provider-specific reasoning effort/variant selected by the caller.
    pub thinking_level: Option<String>,
    /// Provider-specific sandbox/approval/agent mode selected by the caller.
    pub permission_mode: Option<String>,
    /// Optional callback for streaming each line of output in real-time
    pub on_event: Option<OnEventCallback>,
    /// Optional bridge for the interactive control protocol.  When `Some`,
    /// `run_interactive` will register pending permissions here and await
    /// decisions from the resolve endpoint instead of ignoring control_request
    /// lines (milestone 2 fallback).  `None` preserves milestone 2 behaviour.
    pub permission_bridge: Option<Arc<PermissionBridge>>,
    /// Resume a previous Claude CLI session by passing `--resume <session_id>`.
    /// Used by the interactive path to continue a conversation across HTTP turns
    /// (ADR-038 Phase 2, milestone 4, `cli_session_id` continuity).
    /// Only meaningful for `run_interactive`; ignored by `run` and
    /// `continue_conversation`.
    pub resume_session_id: Option<String>,
    /// Ephemeral loopback MCP bridge exposing only this request's scoped tools.
    pub mcp_server: Option<McpServerConfig>,
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

/// Model and reasoning capabilities reported by the installed provider CLI.
/// Both provider-status UI and chat validation consume this shape so a model
/// advertised by a harness cannot be rejected by a separate static catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiCliModelCapability {
    pub id: String,
    pub name: String,
    pub reasoning_options: Vec<String>,
    pub default_reasoning_option: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AiCliModelSnapshot {
    pub models: Vec<AiCliModelCapability>,
    pub refreshed_at: chrono::DateTime<chrono::Utc>,
    pub source: &'static str,
}

#[derive(Clone)]
struct CachedAiCliModels {
    identity: String,
    models: Vec<AiCliModelCapability>,
    refreshed_at: chrono::DateTime<chrono::Utc>,
    expires_at: Instant,
}

const MODEL_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(300);
static MODEL_DISCOVERY_CACHE: OnceLock<tokio::sync::RwLock<HashMap<String, CachedAiCliModels>>> =
    OnceLock::new();

fn model_discovery_cache() -> &'static tokio::sync::RwLock<HashMap<String, CachedAiCliModels>> {
    MODEL_DISCOVERY_CACHE.get_or_init(|| tokio::sync::RwLock::new(HashMap::new()))
}

/// Cache account-aware harness discovery by provider plus CLI/auth identity.
/// A failed refresh preserves the previous successful snapshot so a transient
/// CLI problem never empties every model dropdown.
pub async fn discover_model_capabilities_cached(
    provider: &dyn AiCliProvider,
    identity: String,
    refresh: bool,
) -> AiCliModelSnapshot {
    let provider_id = provider.name().to_string();
    let previous = model_discovery_cache()
        .read()
        .await
        .get(&provider_id)
        .cloned()
        .filter(|cached| cached.identity == identity);
    if !refresh {
        if let Some(cached) = previous
            .as_ref()
            .filter(|cached| cached.expires_at > Instant::now())
        {
            return AiCliModelSnapshot {
                models: cached.models.clone(),
                refreshed_at: cached.refreshed_at,
                source: "cache",
            };
        }
    }

    let discovered = provider.discover_model_capabilities().await;
    if discovered.is_empty() {
        if let Some(cached) = previous {
            return AiCliModelSnapshot {
                models: cached.models,
                refreshed_at: cached.refreshed_at,
                source: "stale_cache",
            };
        }
        return AiCliModelSnapshot {
            models: Vec::new(),
            refreshed_at: chrono::Utc::now(),
            source: "unavailable",
        };
    }

    let refreshed_at = chrono::Utc::now();
    model_discovery_cache().write().await.insert(
        provider_id,
        CachedAiCliModels {
            identity,
            models: discovered.clone(),
            refreshed_at,
            expires_at: Instant::now() + MODEL_DISCOVERY_CACHE_TTL,
        },
    );
    AiCliModelSnapshot {
        models: discovered,
        refreshed_at,
        source: "live",
    }
}

pub async fn invalidate_model_discovery_cache() {
    model_discovery_cache().write().await.clear();
}

pub async fn discover_model_capabilities(provider: &str) -> Vec<AiCliModelCapability> {
    match create_provider(provider) {
        Some(provider) => provider.discover_model_capabilities().await,
        None => Vec::new(),
    }
}

#[async_trait]
pub trait AiCliProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn check_installed(&self) -> bool;
    async fn get_status(&self) -> AiCliStatus;
    async fn discover_model_capabilities(&self) -> Vec<AiCliModelCapability> {
        Vec::new()
    }
    fn extract_assistant_text(&self, _line: &str) -> Option<String> {
        None
    }
    fn extract_partial_text(&self, _line: &str) -> Option<String> {
        None
    }
    fn dropped_tool_use_name(&self, _line: &str) -> Option<String> {
        None
    }

    async fn capabilities(&self) -> Option<temps_ai::ProviderCapabilities> {
        let registration = find_provider(self.name())?;
        let models = self
            .discover_model_capabilities()
            .await
            .into_iter()
            .map(|model| temps_ai::ModelCapability {
                id: model.id,
                name: model.name,
                thinking_modes: model
                    .reasoning_options
                    .into_iter()
                    .map(|id| temps_ai::SelectOption {
                        name: display_option_name(&id),
                        id,
                        description: Some("Supported by this model".to_string()),
                    })
                    .collect(),
                tool_thinking_modes: None,
                default_thinking_mode_id: model.default_reasoning_option,
            })
            .collect::<Vec<_>>();
        Some(temps_ai::ProviderCapabilities {
            id: registration.id.to_string(),
            name: registration.name.to_string(),
            auth_source: temps_ai::ProviderAuthSource::HostEnvironment,
            default_model_id: models.first().map(|model| model.id.clone()),
            models,
            permission_modes: registration
                .permission_modes
                .iter()
                .map(|mode| temps_ai::SelectOption {
                    id: mode.id.to_string(),
                    name: mode.name.to_string(),
                    description: Some(mode.description.to_string()),
                })
                .collect(),
            default_permission_mode_id: Some(registration.default_permission_mode_id.to_string()),
            realtime: temps_ai::RealtimeCapabilities {
                text_streaming: registration.text_streaming,
                reasoning_streaming: registration.reasoning_streaming,
                tool_events: true,
                user_interactions: registration.user_interactions,
                cancellation: true,
            },
        })
    }
    async fn run(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError>;
    /// Execute one normalized chat turn. Most adapters use their regular run
    /// protocol; adapters with a bidirectional interaction protocol override
    /// this without requiring a special path in ConversationService.
    async fn run_turn(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        self.run(config).await
    }
    /// Continue an existing conversation in the same work directory.
    /// Uses `--continue` to resume the most recent session.
    async fn continue_conversation(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError>;
}

fn display_option_name(id: &str) -> String {
    match id {
        "xhigh" => "Extra high".to_string(),
        value => {
            let mut characters = value.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default()
        }
    }
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

/// Extract the human-readable assistant text carried by one line of a
/// provider's structured CLI output, or `None` when the line is a
/// system/hook/tool-call/usage-only event with nothing to show a user.
///
/// Every provider here is invoked with a JSON-lines output flag
/// (`--output-format stream-json`, `--json`, `--format json`) so its stdout
/// is NDJSON, not plain prose — a caller that forwards raw lines as if they
/// were chat tokens ends up displaying the wire protocol (hook events, rate
/// limit frames, tool-call deltas) instead of the answer. Each provider's
/// schema differs, so dispatch on `provider` (an `AiCliProvider::name()`
/// value) the same way [`parse_output`] does.
pub fn extract_assistant_text(provider: &str, line: &str) -> Option<String> {
    create_provider(provider)?.extract_assistant_text(line)
}

/// Extract one incremental text delta from a line, for providers whose CLI
/// exposes real token-by-token streaming — currently only `claude_cli` (via
/// `--include-partial-messages`; see [`claude::extract_partial_text`]).
/// Codex's and OpenCode's `--json`/`--format json` output has no equivalent
/// delta event today, so they always return `None` here — callers must fall
/// back to [`extract_assistant_text`] for those providers, which still
/// delivers the full reply, just not incrementally.
pub fn extract_partial_text(provider: &str, line: &str) -> Option<String> {
    create_provider(provider)?.extract_partial_text(line)
}

/// Name the tool/action a dropped event invoked, when `line` is a tool call
/// or other non-text event that CLI-chat cannot bridge back to the user
/// (ADR-038) — e.g. `AskUserQuestion`, `ExitPlanMode`, `command_execution`.
/// Returns `None` when `line` isn't one of these (including whenever
/// [`extract_assistant_text`] already returned text for it).
///
/// Callers use this only to `warn!`-log the tool name for operator
/// diagnostics — never log the returned name's surrounding `input`/`part`
/// data, which may carry user content or command output.
pub fn dropped_tool_use_name(provider: &str, line: &str) -> Option<String> {
    create_provider(provider)?.dropped_tool_use_name(line)
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
        // OpenCode can exit successfully while reporting a provider/auth
        // failure as a JSON event. Surface its human message instead of
        // treating the run as an empty successful reply.
        if v.get("type").and_then(|t| t.as_str()) == Some("error") {
            let message = v
                .pointer("/error/data/message")
                .or_else(|| v.pointer("/error/message"))
                .and_then(|message| message.as_str());
            if let Some(message) = message.filter(|message| !message.trim().is_empty()) {
                return scrub_and_bound(message);
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
    find_provider(name).map(|registration| (registration.factory)())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_harness_environment_drops_server_secrets() {
        let mut command = Command::new("provider");
        command.env("DATABASE_URL", "postgres://secret");
        command.env("TEMPS_ENCRYPTION_KEY", "secret");
        sanitize_command_environment(&mut command);
        let environment = command
            .as_std()
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(environment.get("DATABASE_URL"), None);
        assert_eq!(environment.get("TEMPS_ENCRYPTION_KEY"), None);
        if std::env::var_os("PATH").is_some() {
            assert!(environment.contains_key("PATH"));
        }
    }

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
    fn test_summarize_opencode_zero_exit_error_frame() {
        let output = r#"{"type":"error","error":{"name":"UnknownError","data":{"message":"Token refresh failed: 401"}}}"#;
        assert_eq!(
            summarize_cli_failure("opencode", output),
            "Token refresh failed: 401"
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
