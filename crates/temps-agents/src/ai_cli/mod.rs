// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// A provider-native action performed by an AI harness.
///
/// These are display events only: the harness has already executed the
/// action in its own workspace. Keeping this separate from [`temps_ai`]'s
/// request-scoped tool protocol lets the chat timeline show real CLI activity
/// such as Claude's `Bash`, `Read`, and `Write` calls without giving the
/// harness additional Temps capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeToolEvent {
    Call {
        id: String,
        name: String,
        arguments: String,
    },
    Result {
        call_id: String,
        result: String,
    },
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

#[derive(Clone)]
struct CachedAiCliStatus {
    status: AiCliStatus,
    expires_at: Instant,
}

const STATUS_CACHE_TTL: Duration = Duration::from_secs(30);
static STATUS_CACHE: OnceLock<tokio::sync::RwLock<HashMap<String, CachedAiCliStatus>>> =
    OnceLock::new();
static STATUS_REFRESH_LOCKS: OnceLock<
    tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
> = OnceLock::new();

fn model_discovery_cache() -> &'static tokio::sync::RwLock<HashMap<String, CachedAiCliModels>> {
    MODEL_DISCOVERY_CACHE.get_or_init(|| tokio::sync::RwLock::new(HashMap::new()))
}

fn status_cache() -> &'static tokio::sync::RwLock<HashMap<String, CachedAiCliStatus>> {
    STATUS_CACHE.get_or_init(|| tokio::sync::RwLock::new(HashMap::new()))
}

fn status_refresh_locks(
) -> &'static tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    STATUS_REFRESH_LOCKS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// Read provider status through a short-lived single-flight cache. Multiple
/// chat surfaces mount together and ask for the same catalog; without this,
/// they launch duplicate CLI auth commands and make the composer wait twice.
pub async fn get_status_cached(
    provider: &dyn AiCliProvider,
    refresh: bool,
    timeout: Duration,
) -> Option<AiCliStatus> {
    let provider_id = provider.name().to_string();
    if !refresh {
        if let Some(status) = status_cache()
            .read()
            .await
            .get(&provider_id)
            .filter(|cached| cached.expires_at > Instant::now())
            .map(|cached| cached.status.clone())
        {
            return Some(status);
        }
    }

    let refresh_lock = {
        let mut locks = status_refresh_locks().lock().await;
        locks
            .entry(provider_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = refresh_lock.lock().await;

    // A concurrent caller may have populated the cache while this call waited.
    if !refresh {
        if let Some(status) = status_cache()
            .read()
            .await
            .get(&provider_id)
            .filter(|cached| cached.expires_at > Instant::now())
            .map(|cached| cached.status.clone())
        {
            return Some(status);
        }
    }

    let status = tokio::time::timeout(timeout, provider.get_status())
        .await
        .ok()?;
    status_cache().write().await.insert(
        provider_id,
        CachedAiCliStatus {
            status: status.clone(),
            expires_at: Instant::now() + STATUS_CACHE_TTL,
        },
    );
    Some(status)
}

/// Return the last host status without invoking the provider CLI. Stale
/// status is preferable to delaying every chat paint; the explicit refresh
/// path replaces it with a live probe.
pub async fn cached_status(provider_id: &str) -> Option<AiCliStatus> {
    status_cache()
        .read()
        .await
        .get(provider_id)
        .map(|cached| cached.status.clone())
}

/// Clear cached provider readiness. Configuration changes and tests use this
/// to ensure the next lookup observes the current host authentication state.
pub async fn invalidate_status_cache() {
    status_cache().write().await.clear();
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

/// Return the most recent account-aware model snapshot without invoking the
/// provider CLI. Catalog reads use this path so rendering the chat composer
/// never blocks on a 10-15 second model-discovery subprocess. Callers that
/// explicitly refresh the catalog use [`discover_model_capabilities_cached`]
/// with `refresh = true` instead.
pub async fn cached_model_capabilities(
    provider_id: &str,
    identity: &str,
) -> Option<AiCliModelSnapshot> {
    let cached = model_discovery_cache()
        .read()
        .await
        .get(provider_id)
        .cloned()
        .filter(|cached| cached.identity == identity)?;
    let source = if cached.expires_at > Instant::now() {
        "cache"
    } else {
        "stale_cache"
    };
    Some(AiCliModelSnapshot {
        models: cached.models,
        refreshed_at: cached.refreshed_at,
        source,
    })
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

/// Build the normalized provider contract from a discovered or bootstrap
/// model list without launching another CLI subprocess.
pub fn provider_capabilities_from_models(
    provider: &str,
    discovered_models: Vec<AiCliModelCapability>,
) -> Option<temps_ai::ProviderCapabilities> {
    let registration = find_provider(provider)?;
    let models = if discovered_models.is_empty() {
        registration
            .models
            .iter()
            .map(|model| AiCliModelCapability {
                id: (*model).to_string(),
                name: display_option_name(model),
                reasoning_options: Vec::new(),
                default_reasoning_option: None,
            })
            .collect()
    } else {
        discovered_models
    };
    let models = models
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
    /// Extract provider-native tool activity from one structured output line.
    ///
    /// The adapter never executes these calls: it only makes already-executed
    /// harness activity visible in Temps' persistent chat timeline.
    fn extract_native_tool_events(&self, _line: &str) -> Vec<NativeToolEvent> {
        Vec::new()
    }
    fn dropped_tool_use_name(&self, _line: &str) -> Option<String> {
        None
    }

    async fn capabilities(&self) -> Option<temps_ai::ProviderCapabilities> {
        provider_capabilities_from_models(self.name(), self.discover_model_capabilities().await)
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

/// Provider-neutral identity and display metadata for one harness session.
/// Values are deliberately bounded before crossing the adapter boundary: a
/// malformed provider event must not turn a navigation label into arbitrary
/// transcript content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionMetadata {
    pub session_id: Option<String>,
    pub title: Option<String>,
}

fn bounded_session_value(value: &str, max_chars: usize) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.chars().take(max_chars).collect())
}

/// Extract session metadata from one structured CLI event.
///
/// Claude emits `session_id` on `system/init` (and sometimes the terminal
/// `result`), Codex emits `thread_id` on `thread.started`, and OpenCode embeds
/// its session id in step parts. Unknown or unrelated events return `None`.
/// Optional title aliases are accepted for forward compatibility; Claude's
/// current stream normally omits one, so the sandbox adapter resolves its
/// provider-owned transcript metadata after the process exits.
pub fn extract_session_metadata(provider: &str, line: &str) -> Option<ProviderSessionMetadata> {
    let value = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
    let event_type = value.get("type").and_then(serde_json::Value::as_str);

    let session_id = match provider {
        "claude_cli" if matches!(event_type, Some("system" | "result")) => {
            value.get("session_id").and_then(serde_json::Value::as_str)
        }
        "codex_cli" if event_type == Some("thread.started") => {
            value.get("thread_id").and_then(serde_json::Value::as_str)
        }
        "opencode" if matches!(event_type, Some("step_start" | "step_finish" | "text")) => value
            .get("sessionID")
            .or_else(|| value.get("sessionId"))
            .or_else(|| value.get("session_id"))
            .or_else(|| value.pointer("/part/sessionID"))
            .or_else(|| value.pointer("/part/sessionId"))
            .and_then(serde_json::Value::as_str),
        _ => None,
    };
    let title = value
        .get("title")
        .or_else(|| value.get("session_title"))
        .or_else(|| value.get("customTitle"))
        .and_then(serde_json::Value::as_str);

    let session_id = session_id.and_then(|value| bounded_session_value(value, 200));
    let title = title.and_then(|value| bounded_session_value(value, 120));
    (session_id.is_some() || title.is_some())
        .then_some(ProviderSessionMetadata { session_id, title })
}

/// Extract tool activity produced by a provider-native CLI. Unknown providers
/// intentionally produce no events so a new adapter cannot accidentally
/// expose an undocumented wire payload to the browser.
pub fn extract_native_tool_events(provider: &str, line: &str) -> Vec<NativeToolEvent> {
    create_provider(provider)
        .map(|provider| provider.extract_native_tool_events(line))
        .unwrap_or_default()
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

/// Redact credential-shaped values without truncating user-visible text.
///
/// This is also applied to sandbox harness output because the process can read
/// its own turn-scoped model and MCP capability values. Those capabilities are
/// short-lived, but they must never become chat content or persisted metadata.
pub fn scrub_secrets(s: &str) -> String {
    // Redact common secret prefixes followed by their token body.
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    const PREFIXES: &[&str] = &[
        "sk-ant-", "sk-", "Bearer ", "ghp_", "gho_", "glpat-", "tmodel_", "tmcp_",
    ];
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

/// Bound an error snippet to 500 chars and redact anything that looks like a
/// credential — CLI errors occasionally echo key fragments, and this string
/// lands in `error_message`, which is returned to any user with read access.
pub fn scrub_and_bound(s: &str) -> String {
    let bounded: String = s.trim().chars().take(500).collect();
    scrub_secrets(&bounded)
}

/// Create an AI CLI provider by name
pub fn create_provider(name: &str) -> Option<Box<dyn AiCliProvider>> {
    find_provider(name).map(|registration| (registration.factory)())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CachedStatusOnlyProvider;

    #[async_trait]
    impl AiCliProvider for CachedStatusOnlyProvider {
        fn name(&self) -> &str {
            "test_cached_status"
        }

        async fn check_installed(&self) -> bool {
            panic!("a cached catalog read must not invoke the CLI")
        }

        async fn get_status(&self) -> AiCliStatus {
            panic!("a cached catalog read must not invoke the CLI")
        }

        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            panic!("not used by this test")
        }

        async fn continue_conversation(
            &self,
            _config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            panic!("not used by this test")
        }
    }

    #[tokio::test]
    async fn cached_status_read_does_not_launch_a_duplicate_cli_probe() {
        status_cache().write().await.insert(
            "test_cached_status".to_string(),
            CachedAiCliStatus {
                status: AiCliStatus {
                    provider: "test_cached_status".to_string(),
                    installed: true,
                    version: Some("1.0.0".to_string()),
                    authenticated: true,
                    auth_method: Some("subscription".to_string()),
                    email: None,
                    subscription_type: None,
                    setup_hint: None,
                },
                expires_at: Instant::now() + Duration::from_secs(60),
            },
        );

        let status = get_status_cached(&CachedStatusOnlyProvider, false, Duration::from_millis(1))
            .await
            .expect("cached provider status");
        assert!(status.authenticated);
        assert_eq!(status.version.as_deref(), Some("1.0.0"));
    }

    #[tokio::test]
    async fn cached_model_lookup_never_requires_live_discovery() {
        let provider_id = "test_cached_model_lookup";
        let identity = "v1|subscription|user@example.test|pro";
        let refreshed_at = chrono::Utc::now();
        model_discovery_cache().write().await.insert(
            provider_id.to_string(),
            CachedAiCliModels {
                identity: identity.to_string(),
                models: vec![AiCliModelCapability {
                    id: "model-1".to_string(),
                    name: "Model 1".to_string(),
                    reasoning_options: vec!["high".to_string()],
                    default_reasoning_option: Some("high".to_string()),
                }],
                refreshed_at,
                expires_at: Instant::now() + Duration::from_secs(60),
            },
        );

        let snapshot = cached_model_capabilities(provider_id, identity)
            .await
            .expect("cached model snapshot");
        assert_eq!(snapshot.source, "cache");
        assert_eq!(snapshot.models[0].id, "model-1");
        assert_eq!(snapshot.refreshed_at, refreshed_at);
        assert!(cached_model_capabilities(provider_id, "different-user")
            .await
            .is_none());
    }

    #[test]
    fn empty_discovery_uses_the_complete_registered_model_catalog() {
        let capabilities = provider_capabilities_from_models("codex_cli", Vec::new())
            .expect("Codex capability contract");

        assert_eq!(
            capabilities.default_model_id.as_deref(),
            Some("gpt-5.6-sol")
        );
        assert!(capabilities.models.len() > 1);
        assert!(capabilities
            .models
            .iter()
            .any(|model| model.id == "gpt-5.6-terra"));
    }

    #[test]
    fn discovered_models_keep_provider_reasoning_metadata() {
        let capabilities = provider_capabilities_from_models(
            "claude_cli",
            vec![AiCliModelCapability {
                id: "sonnet".to_string(),
                name: "Sonnet".to_string(),
                reasoning_options: vec!["low".to_string(), "high".to_string()],
                default_reasoning_option: Some("high".to_string()),
            }],
        )
        .expect("Claude capability contract");

        assert_eq!(capabilities.models.len(), 1);
        assert_eq!(capabilities.models[0].thinking_modes.len(), 2);
        assert_eq!(
            capabilities.models[0].default_thinking_mode_id.as_deref(),
            Some("high")
        );
    }

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
        assert_eq!(
            scrub_and_bound("relay tmodel_abc123 and tool tmcp_def456"),
            "relay [redacted] and tool [redacted]"
        );
    }

    #[test]
    fn test_scrub_secrets_preserves_long_chat_text() {
        let text = format!("{} tmodel_secret tail", "x".repeat(700));
        let scrubbed = scrub_secrets(&text);
        assert_eq!(scrubbed.len(), 700 + " [redacted] tail".len());
        assert!(!scrubbed.contains("tmodel_secret"));
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

    #[test]
    fn extracts_claude_session_metadata_without_exposing_other_events() {
        let metadata = extract_session_metadata(
            "claude_cli",
            r#"{"type":"system","subtype":"init","session_id":"session-1"}"#,
        )
        .expect("Claude init metadata");
        assert_eq!(metadata.session_id.as_deref(), Some("session-1"));
        assert_eq!(metadata.title, None);
        assert!(extract_session_metadata(
            "claude_cli",
            r#"{"type":"assistant","session_id":"do-not-trust"}"#
        )
        .is_none());
    }

    #[test]
    fn normalizes_codex_and_opencode_session_metadata() {
        let codex = extract_session_metadata(
            "codex_cli",
            r#"{"type":"thread.started","thread_id":"thread-1","title":"  Deploy   Storefront  "}"#,
        )
        .expect("Codex thread metadata");
        assert_eq!(codex.session_id.as_deref(), Some("thread-1"));
        assert_eq!(codex.title.as_deref(), Some("Deploy Storefront"));

        let opencode = extract_session_metadata(
            "opencode",
            r#"{"type":"step_start","part":{"sessionID":"session-2"}}"#,
        )
        .expect("OpenCode step metadata");
        assert_eq!(opencode.session_id.as_deref(), Some("session-2"));
    }
}
