// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`AgentCliAiService`]: an [`AiService`] implementation that delegates
//! eligible workloads to a subscription-backed agent CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use tokio::sync::Semaphore;

use temps_agents::ai_cli::{
    cached_model_capabilities, discover_model_capabilities_cached, extract_session_metadata,
    get_status_cached, provider_capabilities_from_models, scrub_and_bound, scrub_secrets,
    AiCliProvider, AiRunConfig, OnEventCallback, PermissionBridge, ProviderSessionMetadata,
};
use temps_agents::error::AgentError;
use temps_agents::sandbox::{SandboxCreateConfig, SandboxProvider};
use temps_ai::{
    extract_json_block, AiError, AiRequest, AiResponse, AiService, ChatStreamDelta, ChatTool,
    ChatTurnRequest, ChatTurnStream, TokenStream, ToolCall, ToolExecutor, TurnServices,
};

use crate::model_relay::{SandboxHarnessCredentials, SandboxModelRelay, SandboxModelRelayService};

#[derive(Clone)]
struct McpBridgeState {
    token: String,
    tools: Arc<Vec<ChatTool>>,
    executor: ToolExecutor,
    events: tokio::sync::mpsc::Sender<Result<ChatStreamDelta, AiError>>,
    tool_slot: Arc<Semaphore>,
    tool_timeout: Duration,
}

const MCP_EVENT_CAPACITY: usize = 32;
const MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MCP_BODY_LIMIT_BYTES: usize = 1024 * 1024;

/// Constant-time equality for the bridge's bearer token check.
///
/// Same primitive `temps-routes` uses for its route-sync token check
/// (`subtle::ConstantTimeEq`), applied here as defense-in-depth: the bridge
/// already binds to loopback on an ephemeral port behind a random per-instance
/// path and token, but a length-preserving `==` still leaks a timing signal.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

fn build_bridge_router(path: &str, state: McpBridgeState) -> axum::Router {
    axum::Router::new()
        .route(path, axum::routing::post(mcp_bridge_handler))
        .layer(axum::extract::DefaultBodyLimit::max(MCP_BODY_LIMIT_BYTES))
        .with_state(state)
}

const PROVIDER_STATUS_TIMEOUT: Duration = Duration::from_secs(5);

type NativeToolCalls = Arc<Mutex<HashMap<String, ToolCall>>>;

fn merge_session_metadata(
    slot: &Arc<Mutex<Option<ProviderSessionMetadata>>>,
    incoming: ProviderSessionMetadata,
) {
    let mut metadata = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match metadata.as_mut() {
        Some(current) => {
            if incoming.session_id.is_some() {
                current.session_id = incoming.session_id;
            }
            if incoming.title.is_some() {
                current.title = incoming.title;
            }
        }
        None => *metadata = Some(incoming),
    }
}

fn bounded_harness_title(value: &str) -> Option<String> {
    const MAX_CHARS: usize = 120;
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.chars().take(MAX_CHARS).collect())
}

/// Claude currently stores its human-facing session label in local metadata
/// rather than emitting it on `stream-json`. Newer indexes may write a
/// `custom-title`; loose transcripts expose the first prompt. Temps sends a
/// flattened role transcript as that prompt, so take its final `[user]`
/// segment instead of using the system framing as a navigation label.
fn claude_session_title_from_transcript(contents: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(contents).ok()?;
    let mut first_prompt: Option<String> = None;
    for line in text.lines().take(40) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) == Some("custom-title") {
            if let Some(title) = value
                .get("customTitle")
                .and_then(serde_json::Value::as_str)
                .and_then(bounded_harness_title)
            {
                return Some(title);
            }
        }
        if first_prompt.is_none()
            && value.get("type").and_then(serde_json::Value::as_str) == Some("user")
            && value
                .pointer("/message/role")
                .and_then(serde_json::Value::as_str)
                == Some("user")
        {
            first_prompt = value
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        (item.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                            .then(|| item.get("text").and_then(serde_json::Value::as_str))
                            .flatten()
                    })
                })
                .or_else(|| {
                    value
                        .pointer("/message/content")
                        .and_then(serde_json::Value::as_str)
                })
                .map(str::to_string);
        }
    }
    let prompt = first_prompt?;
    let user_prompt = prompt
        .rsplit("\n\n[user]\n")
        .next()
        .unwrap_or(prompt.as_str());
    let user_prompt = user_prompt
        .split("\n\n[assistant]\n")
        .next()
        .unwrap_or(user_prompt);
    bounded_harness_title(user_prompt)
}

async fn resolve_sandbox_session_title(
    provider_name: &str,
    sandbox: &dyn SandboxProvider,
    handle: &temps_agents::sandbox::SandboxHandle,
    session_id: &str,
) -> Option<String> {
    if provider_name != "claude_cli"
        || session_id.is_empty()
        || session_id.len() > 200
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return None;
    }
    let path = format!("/home/temps/.claude/projects/-home-temps-workspace/{session_id}.jsonl");
    match sandbox.read_file(handle, &path).await {
        Ok(contents) => claude_session_title_from_transcript(&contents),
        Err(error) => {
            tracing::debug!(
                provider = provider_name,
                %session_id,
                %error,
                "provider session title metadata was unavailable"
            );
            None
        }
    }
}

struct StreamingSecretRedactor {
    secrets: Vec<String>,
    pending: String,
}

impl StreamingSecretRedactor {
    fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            secrets: secrets
                .into_iter()
                .filter(|secret| !secret.is_empty())
                .collect(),
            pending: String::new(),
        }
    }

    fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut redacted = String::new();

        loop {
            let earliest = self
                .secrets
                .iter()
                .filter_map(|secret| self.pending.find(secret).map(|index| (index, secret.len())))
                .min_by_key(|(index, _)| *index);
            let Some((index, secret_len)) = earliest else {
                break;
            };
            redacted.push_str(&self.pending[..index]);
            redacted.push_str("[redacted]");
            self.pending.drain(..index + secret_len);
        }

        let retained = self
            .secrets
            .iter()
            .flat_map(|secret| {
                secret
                    .char_indices()
                    .map(|(index, _)| index)
                    .chain(std::iter::once(secret.len()))
                    .filter(|prefix_len| {
                        *prefix_len > 0 && self.pending.ends_with(&secret[..*prefix_len])
                    })
            })
            .max()
            .unwrap_or(0);
        let emit_len = self.pending.len().saturating_sub(retained);
        redacted.push_str(&self.pending[..emit_len]);
        self.pending.drain(..emit_len);
        scrub_secrets(&redacted)
    }

    fn finish(&mut self) -> String {
        scrub_secrets(&std::mem::take(&mut self.pending))
    }
}

fn scrub_native_tool_arguments(arguments: &str) -> String {
    fn scrub_value(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(value) => serde_json::Value::String(scrub_and_bound(value)),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.iter().map(scrub_value).collect())
            }
            serde_json::Value::Object(values) => serde_json::Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), scrub_value(value)))
                    .collect(),
            ),
            value => value.clone(),
        }
    }

    serde_json::from_str::<serde_json::Value>(arguments)
        .map(|value| scrub_value(&value))
        .and_then(|value| serde_json::to_string(&value))
        // Keep an unknown provider's malformed wire payload safe too; the UI
        // renders it as plain text rather than trying to execute it.
        .unwrap_or_else(|_| scrub_and_bound(arguments))
}

/// Turn one harness' structured activity into the same deltas used for MCP
/// tools. The call map makes a later native result render against the exact
/// call that produced it, while deduplicating Claude's occasional replay of a
/// completed assistant event.
fn native_tool_deltas(
    provider: &dyn AiCliProvider,
    line: &str,
    calls: &NativeToolCalls,
) -> Vec<ChatStreamDelta> {
    let mut deltas = Vec::new();
    for event in provider.extract_native_tool_events(line) {
        match event {
            temps_agents::ai_cli::NativeToolEvent::Call {
                id,
                name,
                arguments,
            } => {
                let call = ToolCall {
                    id: id.clone(),
                    name,
                    // Native tool input can include a command with an inline
                    // credential. Bound and scrub it before it reaches SSE or
                    // message metadata; the harness still receives the raw
                    // input independently inside its own process.
                    arguments: scrub_native_tool_arguments(&arguments),
                };
                let inserted = match calls.lock() {
                    Ok(mut calls) => calls.insert(id, call.clone()).is_none(),
                    Err(poisoned) => poisoned.into_inner().insert(id, call.clone()).is_none(),
                };
                if inserted {
                    deltas.push(ChatStreamDelta::ToolCall(call));
                }
            }
            temps_agents::ai_cli::NativeToolEvent::Result { call_id, result } => {
                let call = match calls.lock() {
                    Ok(calls) => calls.get(&call_id).cloned(),
                    Err(poisoned) => poisoned.into_inner().get(&call_id).cloned(),
                };
                if let Some(call) = call {
                    deltas.push(ChatStreamDelta::ToolResult {
                        call,
                        result: scrub_and_bound(&result),
                    });
                }
            }
        }
    }
    deltas
}

/// Resolves the encrypted Agent Sandbox credential for one selected harness.
/// Implemented by the composition root so this crate never reads settings or
/// encryption keys directly.
pub type SandboxCredentialResolver = Arc<
    dyn Fn(&str) -> BoxFuture<'static, Result<SandboxHarnessCredentials, AiError>> + Send + Sync,
>;

async fn mcp_bridge_handler(
    axum::extract::State(state): axum::extract::State<McpBridgeState>,
    headers: axum::http::HeaderMap,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    let authorized = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            constant_time_eq(
                value.as_bytes(),
                format!("Bearer {}", state.token).as_bytes(),
            )
        });
    if !authorized {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let method = request
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    // JSON-RPC notifications deliberately have no response. Native MCP
    // clients send `notifications/initialized` immediately after initialize.
    if request.get("id").is_none() {
        return axum::http::StatusCode::ACCEPTED.into_response();
    }
    let response = match method {
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "temps-chat", "version": "1"}
            }
        }),
        "tools/list" => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"tools": state.tools.iter().map(|tool| serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.parameters
            })).collect::<Vec<_>>()}
        }),
        "ping" => serde_json::json!({
            "jsonrpc": "2.0", "id": id, "result": {}
        }),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_default();
            let name = params
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let known = state.tools.iter().any(|tool| tool.name == name);
            if !known {
                serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {"content": [{"type": "text", "text": "Tool is not available for this conversation"}], "isError": true}
                })
            } else {
                let _tool_permit = match state.tool_slot.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        return (
                            axum::http::StatusCode::OK,
                            axum::Json(serde_json::json!({
                                "jsonrpc": "2.0", "id": id,
                                "result": {"content": [{"type": "text", "text": "Another Temps tool call is already running for this turn"}], "isError": true}
                            })),
                        )
                            .into_response();
                    }
                };
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let call = ToolCall {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                };
                let _ = state
                    .events
                    .send(Ok(ChatStreamDelta::ToolCall(call.clone())))
                    .await;
                // Every claimed native call gets a terminal ToolResult, including
                // failures. The common conversation loop uses this event to mark
                // the call handled; omitting it would make the fallback dispatcher
                // execute the same write proposal a second time.
                let (result, is_error) =
                    match tokio::time::timeout(state.tool_timeout, (state.executor)(call.clone()))
                        .await
                    {
                        Err(_) => (
                            format!(
                                "Temps tool '{}' timed out after {}s",
                                call.name,
                                state.tool_timeout.as_secs_f64()
                            ),
                            true,
                        ),
                        Ok(Ok(result)) => (result, false),
                        Ok(Err(error)) => (error.to_string(), true),
                    };
                let _ = state
                    .events
                    .send(Ok(ChatStreamDelta::ToolResult {
                        call,
                        result: result.clone(),
                    }))
                    .await;
                serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {"content": [{"type": "text", "text": result}], "isError": is_error}
                })
            }
        }
        _ => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": "Method not found"}
        }),
    };
    (axum::http::StatusCode::OK, axum::Json(response)).into_response()
}

pub struct ScopedMcpBridge {
    pub config: temps_agents::ai_cli::McpServerConfig,
    pub events: tokio::sync::mpsc::Receiver<Result<ChatStreamDelta, AiError>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl ScopedMcpBridge {
    pub async fn start(tools: Vec<ChatTool>, executor: ToolExecutor) -> Result<Self, AiError> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| AiError::Provider {
                purpose: "chat.tools.bridge".to_string(),
                reason: format!("failed to bind scoped MCP bridge: {error}"),
            })?;
        let address = listener.local_addr().map_err(|error| AiError::Provider {
            purpose: "chat.tools.bridge".to_string(),
            reason: format!("failed to resolve scoped MCP bridge address: {error}"),
        })?;
        let path = format!("/mcp/{}", uuid::Uuid::new_v4().simple());
        let token = uuid::Uuid::new_v4().simple().to_string();
        let url = format!("http://{address}{path}");
        let (events_tx, events) = tokio::sync::mpsc::channel(MCP_EVENT_CAPACITY);
        let state = McpBridgeState {
            token: token.clone(),
            tools: Arc::new(tools),
            executor,
            events: events_tx,
            tool_slot: Arc::new(Semaphore::new(1)),
            tool_timeout: MCP_TOOL_TIMEOUT,
        };
        let router = build_bridge_router(&path, state);
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        Ok(Self {
            config: temps_agents::ai_cli::McpServerConfig {
                url,
                authorization_token: token,
            },
            events,
            shutdown: Some(shutdown),
            task,
        })
    }

    pub async fn shutdown(mut self) {
        self.shutdown_with_timeout(MCP_SHUTDOWN_TIMEOUT).await;
    }

    async fn shutdown_with_timeout(&mut self, timeout: Duration) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if tokio::time::timeout(timeout, &mut self.task).await.is_err() {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }

    pub fn take_events(&mut self) -> tokio::sync::mpsc::Receiver<Result<ChatStreamDelta, AiError>> {
        let (_sender, receiver) = tokio::sync::mpsc::channel(1);
        std::mem::replace(&mut self.events, receiver)
    }
}

impl Drop for ScopedMcpBridge {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        // A cancelled/panicked provider turn must not leave the authenticated
        // listener alive. Normal completion uses `shutdown()` and awaits it.
        self.task.abort();
    }
}

/// Couples a spawned provider turn to the stream returned to its caller.
/// Dropping the stream aborts the task; dropping the provider future then
/// drops its `kill_on_drop` child process instead of leaving it detached.
struct AbortTaskOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Request-scoped owner for provider interaction waiters. A harness can block
/// on a question or approval while its output stream is open; when that stream
/// is cancelled, every waiter must be cancelled with the harness turn.
struct InteractionTaskOwner(Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>);

impl Drop for InteractionTaskOwner {
    fn drop(&mut self) {
        let mut tasks = match self.0.lock() {
            Ok(tasks) => tasks,
            Err(poisoned) => poisoned.into_inner(),
        };
        for task in tasks.drain(..) {
            task.abort();
        }
    }
}

/// Hard cap on the flattened prompt size sent to an agent CLI subprocess.
/// Without this, a caller-controlled `AiRequest`/`ChatTurnRequest` could hold
/// a semaphore permit for the full timeout window with a multi-MB prompt,
/// pressuring subprocess memory and starving other tenants of the small
/// (default 2) concurrency budget.
const MAX_PROMPT_BYTES: usize = 32 * 1024;

/// An [`AiService`] implementation that delegates eligible workloads to an
/// [`AiCliProvider`] (Claude Code, Codex, OpenCode).
///
/// # Subscription mode
///
/// `api_key` in every host [`AiRunConfig`] is deliberately `""`; host CLI
/// authentication stays in that CLI's standard config. Application turns use
/// a different path: Temps resolves the encrypted provider credential into a
/// host-side relay and gives the sandbox only an expiring relay capability.
///
/// Multi-turn tool calls are delegated only when the conversation layer
/// supplies a scoped executor. The service exposes that executor to the CLI
/// through an authenticated, per-turn loopback MCP endpoint and never embeds
/// project credentials in tool arguments or results.
pub struct AgentCliAiService {
    provider: Arc<dyn AiCliProvider>,
    /// Root directory for per-invocation tempdirs. Must exist before any call.
    scratch_dir: PathBuf,
    /// Hard deadline for every `provider.run()` call (default: 30s).
    timeout: Duration,
    /// Limits concurrent CLI subprocesses on the host.
    concurrency: Arc<Semaphore>,
    /// The instance sandbox boundary for application harnesses. When a chat
    /// supplies `harness_workspace`, execution is required to use this
    /// provider; it must never fall back to the host CLI process.
    sandbox_provider: Option<Arc<dyn SandboxProvider>>,
    /// Parent of every trusted application workspace. This guards the
    /// in-process request seam too: even a future caller cannot mount an
    /// arbitrary server directory by constructing `ChatTurnRequest` directly.
    sandbox_workspace_root: Option<PathBuf>,
    sandbox_credentials: Option<SandboxCredentialResolver>,
    /// Host-side provider relay. The sandbox receives only its short-lived
    /// capability; real provider credentials never cross the boundary.
    sandbox_model_relay: Option<Arc<SandboxModelRelayService>>,
    /// Handles recovered by this runtime. Recovering a Docker sandbox also
    /// performs recursive ownership repair for legacy/root-owned workspaces;
    /// repeating that on every chat message can walk a large node_modules tree
    /// twice. A cached handle still gets a cheap liveness check before reuse.
    sandbox_handles: Arc<Mutex<HashMap<String, temps_agents::sandbox::SandboxHandle>>>,
    /// A persistent application sandbox may host multiple conversations, but
    /// only one harness process may mutate it at a time.
    sandbox_slots: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    /// Development turns include package installation, first-run compilation,
    /// and occasionally a test suite. They are bounded again by the chat
    /// service's configurable turn deadline, so they must not inherit the
    /// short host-CLI completion timeout used for gateway-adjacent jobs.
    sandbox_timeout: Option<Duration>,
}

/// If a server-owned turn is aborted while Docker exec is still active, stop
/// the container. The application workspace is a host bind mount, so stopping
/// preserves project data while guaranteeing the orphaned harness and every
/// turn capability disappear before the next turn restarts the sandbox.
struct StopSandboxOnDrop {
    armed: bool,
    provider: Arc<dyn SandboxProvider>,
    handle: temps_agents::sandbox::SandboxHandle,
    handles: Arc<Mutex<HashMap<String, temps_agents::sandbox::SandboxHandle>>>,
    sandbox_label: String,
    sandbox_slot: Arc<Semaphore>,
    global_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    sandbox_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl StopSandboxOnDrop {
    fn disarm(&mut self) {
        self.armed = false;
        self.global_permit.take();
        self.sandbox_permit.take();
    }

    async fn stop_now(&mut self) {
        if !self.armed {
            return;
        }
        self.handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.sandbox_label);
        match self.provider.stop(&self.handle).await {
            Ok(()) => {}
            Err(error) => {
                self.sandbox_slot.close();
                tracing::error!(
                    sandbox_label = self.sandbox_label,
                    sandbox_id = %self.handle.sandbox_id,
                    %error,
                    "failed to stop a timed-out application harness sandbox; quarantined it from future turns"
                );
            }
        }
        self.armed = false;
        self.global_permit.take();
        self.sandbox_permit.take();
    }
}

impl Drop for StopSandboxOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.sandbox_label);
        let provider = self.provider.clone();
        let handle = self.handle.clone();
        let sandbox_label = self.sandbox_label.clone();
        let sandbox_slot = self.sandbox_slot.clone();
        let global_permit = self.global_permit.take();
        let sandbox_permit = self.sandbox_permit.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = provider.stop(&handle).await {
                    sandbox_slot.close();
                    tracing::error!(
                        sandbox_label,
                        sandbox_id = %handle.sandbox_id,
                        %error,
                        "failed to stop a cancelled application harness sandbox; quarantined it from future turns"
                    );
                }
                drop(global_permit);
                drop(sandbox_permit);
            });
        } else {
            tracing::error!(
                sandbox_label,
                sandbox_id = %handle.sandbox_id,
                "could not schedule application sandbox stop because no Tokio runtime is active"
            );
        }
    }
}

impl AgentCliAiService {
    /// Create a new service.
    ///
    /// `concurrency_limit` caps how many CLI subprocesses may run concurrently
    /// on the host (ADR-037 §5 recommends 2). `timeout` applies to every
    /// `provider.run()` invocation (ADR-037 §5 recommends 30s).
    ///
    /// # Panics
    ///
    /// Panics if `concurrency_limit` is `0`. A zero-capacity semaphore would
    /// make every call fail with "concurrency limit reached" silently — this
    /// is a misconfiguration that must fail loudly at construction, not
    /// degrade into a service that appears registered but never runs.
    pub fn new(
        provider: Arc<dyn AiCliProvider>,
        scratch_dir: PathBuf,
        timeout: Duration,
        concurrency_limit: usize,
    ) -> Self {
        assert!(
            concurrency_limit > 0,
            "AgentCliAiService concurrency_limit must be at least 1, got 0"
        );
        Self {
            provider,
            scratch_dir,
            timeout,
            concurrency: Arc::new(Semaphore::new(concurrency_limit)),
            sandbox_provider: None,
            sandbox_workspace_root: None,
            sandbox_credentials: None,
            sandbox_model_relay: None,
            sandbox_handles: Arc::new(Mutex::new(HashMap::new())),
            sandbox_slots: Arc::new(Mutex::new(HashMap::new())),
            sandbox_timeout: None,
        }
    }

    /// Enable durable execution for application harness turns. The caller
    /// passes the instance's shared sandbox provider and the data-dir root
    /// (`<TEMPS_DATA_DIR>/ai-applications`), not a per-request directory.
    pub fn with_temps_sandbox(
        mut self,
        sandbox_provider: Arc<dyn SandboxProvider>,
        sandbox_workspace_root: PathBuf,
        sandbox_credentials: SandboxCredentialResolver,
        sandbox_model_relay: Arc<SandboxModelRelayService>,
    ) -> Self {
        self.sandbox_provider = Some(sandbox_provider);
        self.sandbox_workspace_root = Some(sandbox_workspace_root);
        self.sandbox_credentials = Some(sandbox_credentials);
        self.sandbox_model_relay = Some(sandbox_model_relay);
        self.sandbox_timeout = Some(Duration::from_secs(15 * 60));
        self
    }

    fn validate_sandbox_workspace(
        &self,
        workspace: &temps_ai::HarnessWorkspace,
    ) -> Result<(), AiError> {
        let valid_label = !workspace.sandbox_label.is_empty()
            && workspace.sandbox_label.len() <= 200
            && workspace
                .sandbox_label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
        let Some(root) = &self.sandbox_workspace_root else {
            return Err(AiError::Provider {
                purpose: "chat.application".to_string(),
                reason: "Temps sandbox execution is not configured for this instance".to_string(),
            });
        };
        if !valid_label || !is_child_path(root, &workspace.host_work_dir) {
            return Err(AiError::Provider {
                purpose: "chat.application".to_string(),
                reason: "application harness workspace is not a managed Temps data directory"
                    .to_string(),
            });
        }
        Ok(())
    }

    fn cached_sandbox_handle(
        &self,
        sandbox_label: &str,
    ) -> Option<temps_agents::sandbox::SandboxHandle> {
        self.sandbox_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(sandbox_label)
            .cloned()
    }

    fn cache_sandbox_handle(
        &self,
        sandbox_label: String,
        handle: temps_agents::sandbox::SandboxHandle,
    ) {
        self.sandbox_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(sandbox_label, handle);
    }

    fn evict_sandbox_handle(&self, sandbox_label: &str) {
        self.sandbox_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(sandbox_label);
    }

    fn sandbox_slot(&self, sandbox_label: &str) -> Arc<Semaphore> {
        self.sandbox_slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(sandbox_label.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }

    async fn get_or_create_sandbox(
        &self,
        workspace: &temps_ai::HarnessWorkspace,
        project_id: Option<i32>,
    ) -> Result<temps_agents::sandbox::SandboxHandle, AiError> {
        self.validate_sandbox_workspace(workspace)?;
        let provider = self
            .sandbox_provider
            .as_ref()
            .ok_or_else(|| AiError::Provider {
                purpose: "chat.application".to_string(),
                reason: "Temps sandbox execution is not configured for this instance".to_string(),
            })?;

        let cached = self.cached_sandbox_handle(&workspace.sandbox_label);
        if let Some(handle) = cached {
            if provider
                .is_alive(&handle)
                .await
                .map_err(|error| map_agent_error("chat.application.sandbox", error))?
            {
                return Ok(handle);
            }
            self.evict_sandbox_handle(&workspace.sandbox_label);
        }

        let handle = match provider
            .recover_by_name(&workspace.sandbox_label)
            .await
            .map_err(|error| map_agent_error("chat.application.sandbox", error))?
        {
            Some(handle)
                if provider
                    .is_alive(&handle)
                    .await
                    .map_err(|error| map_agent_error("chat.application.sandbox", error))? =>
            {
                // A server restart loses in-memory turn guards. Restart a
                // recovered live container once before reuse so a Docker exec
                // orphan from the previous process cannot keep mutating the
                // persistent workspace or retain an expired capability.
                provider
                    .restart(&handle)
                    .await
                    .map_err(|error| map_agent_error("chat.application.sandbox", error))?;
                Ok(handle)
            }
            Some(handle) => {
                provider
                    .start(&handle)
                    .await
                    .map_err(|error| map_agent_error("chat.application.sandbox", error))?;
                Ok(handle)
            }
            None => provider
                .create(SandboxCreateConfig {
                    // Container identity comes exclusively from the opaque
                    // label. The numeric id is retained for provider error
                    // context and intentionally carries no user input.
                    run_id: project_id.unwrap_or_default(),
                    container_name_override: Some(workspace.sandbox_label.clone()),
                    host_work_dir: workspace.host_work_dir.clone(),
                    // Keep the project files in TEMPS_DATA_DIR, not only in a
                    // Docker volume. The sandbox is durable, but the host data
                    // directory remains the authoritative user-owned copy.
                    workspace_volume: None,
                    image: None,
                    cpu_limit: None,
                    memory_limit_mb: None,
                    pids_limit: None,
                    disk_size_mb: None,
                    network_mode: None,
                    // No host credential, API key, or browser value crosses
                    // this boundary. A harness authenticates inside its own
                    // sandbox, through its normal secure login flow.
                    env_vars: HashMap::new(),
                    idle_timeout: Duration::from_secs(60 * 60),
                    backend: None,
                    owner_user_id: None,
                })
                .await
                .map_err(|error| map_agent_error("chat.application.sandbox", error)),
        }?;
        self.cache_sandbox_handle(workspace.sandbox_label.clone(), handle.clone());
        Ok(handle)
    }

    async fn sandbox_chat_stream_turn(
        &self,
        request: ChatTurnRequest,
    ) -> Result<ChatTurnStream, AiError> {
        let timing_started = Instant::now();
        let trace_id = request
            .trace_id
            .clone()
            .unwrap_or_else(|| "untracked".to_string());
        let workspace = request
            .harness_workspace
            .clone()
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "development harness requests require a managed workspace".to_string(),
            })?;
        if !request.tools.is_empty() && request.harness_mcp_server.is_none() {
            return Err(AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "sandboxed harness platform tools require a turn-scoped MCP capability"
                    .to_string(),
            });
        }
        let prompt = build_sandbox_chat_prompt(&request);
        check_prompt_size(&request.purpose, &prompt)?;
        tracing::info!(
            component = "ai_turn_timing",
            turn_id = %trace_id,
            provider = %self.provider.name(),
            phase = "sandbox_adapter_started",
            message_count = request.messages.len(),
            prompt_bytes = prompt.len(),
            native_session_resumed = request.resume_session_id.is_some(),
            model = request.model.as_deref().unwrap_or("default"),
            thinking_level = request.thinking_level.as_deref().unwrap_or("default"),
            total_ms = timing_started.elapsed().as_millis() as u64,
            "AI turn timing"
        );
        let permit = Arc::clone(&self.concurrency)
            .try_acquire_owned()
            .map_err(|_| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "sandboxed harness concurrency limit reached — try again shortly"
                    .to_string(),
            })?;
        let sandbox_slot = self.sandbox_slot(&workspace.sandbox_label);
        let sandbox_permit =
            sandbox_slot
                .clone()
                .try_acquire_owned()
                .map_err(|_| AiError::Provider {
                    purpose: request.purpose.clone(),
                    reason: "another harness turn is already running in this application sandbox"
                        .to_string(),
                })?;
        let phase_started = Instant::now();
        let handle = self
            .get_or_create_sandbox(&workspace, request.project_id)
            .await?;
        tracing::info!(
            component = "ai_turn_timing",
            turn_id = %trace_id,
            provider = %self.provider.name(),
            phase = "sandbox_ready",
            phase_ms = phase_started.elapsed().as_millis() as u64,
            total_ms = timing_started.elapsed().as_millis() as u64,
            "AI turn timing"
        );
        let phase_started = Instant::now();
        let credentials = self
            .sandbox_credentials
            .as_ref()
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "sandboxed harness credentials are not configured for this instance"
                    .to_string(),
            })?(self.provider.name())
        .await?;
        tracing::info!(
            component = "ai_turn_timing",
            turn_id = %trace_id,
            provider = %self.provider.name(),
            phase = "credentials_resolved",
            phase_ms = phase_started.elapsed().as_millis() as u64,
            total_ms = timing_started.elapsed().as_millis() as u64,
            "AI turn timing"
        );
        let timeout = self.sandbox_timeout.unwrap_or(self.timeout);
        let relay_service = self
            .sandbox_model_relay
            .as_ref()
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "sandboxed harness model relay is not configured for this instance"
                    .to_string(),
            })?;
        let sandbox_provider = self
            .sandbox_provider
            .as_ref()
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "sandboxed harness execution is not configured for this instance"
                    .to_string(),
            })?;
        let relay_base_url = sandbox_provider
            .model_relay_base_url(&handle, &credentials.internal_api_url)
            .await
            .map_err(|error| map_agent_error(&request.purpose, error))?;
        let harness_mcp_server = match request.harness_mcp_server.as_ref() {
            Some(server) => Some(temps_ai::HarnessMcpServer {
                url: sandbox_provider
                    .harness_mcp_url(&handle, &credentials.internal_api_url, &server.url)
                    .await
                    .map_err(|error| map_agent_error(&request.purpose, error))?,
                authorization_token: server.authorization_token.clone(),
            }),
            None => None,
        };
        let (model_relay, model_relay_guard) = relay_service.register(
            self.provider.name(),
            request.principal_id.ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "sandboxed harness turn is missing its authenticated principal".to_string(),
            })?,
            request.model.as_deref(),
            credentials,
            &relay_base_url,
            timeout + Duration::from_secs(30),
        )?;
        let mut streamed_secrets = vec![model_relay.bearer.clone()];
        streamed_secrets.extend(request.sandbox_environment.redaction_values().cloned());
        if let Some(server) = request.harness_mcp_server.as_ref() {
            streamed_secrets.push(server.authorization_token.clone());
        }
        let stream_redactor = Arc::new(Mutex::new(StreamingSecretRedactor::new(streamed_secrets)));
        let mut command = sandbox_harness_command(self.provider.name(), &prompt, &request)?;
        let mut sandbox_env = request.sandbox_environment.clone().into_inner();
        configure_sandbox_model_relay(self.provider.name(), &mut sandbox_env, &model_relay)?;
        let mut turn_secret_files = Vec::new();
        let mcp_secret_path = configure_sandbox_mcp(
            self.provider.name(),
            &mut command,
            &mut sandbox_env,
            &mut turn_secret_files,
            harness_mcp_server.as_ref(),
        )?;
        let sandbox = self
            .sandbox_provider
            .clone()
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "Temps sandbox execution is not configured for this instance".to_string(),
            })?;
        let credential_file_count = turn_secret_files.len();
        let phase_started = Instant::now();
        for (path, contents) in &turn_secret_files {
            sandbox
                .write_file(&handle, path, contents, 0o600)
                .await
                .map_err(|error| map_agent_error("chat.application.sandbox", error))?;
        }
        tracing::info!(
            component = "ai_turn_timing",
            turn_id = %trace_id,
            provider = %self.provider.name(),
            phase = "credentials_seeded",
            credential_file_count,
            phase_ms = phase_started.elapsed().as_millis() as u64,
            total_ms = timing_started.elapsed().as_millis() as u64,
            "AI turn timing"
        );
        let provider = self.provider.clone();
        let purpose = request.purpose.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ChatStreamDelta, AiError>>(64);
        let emitted_partial = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_raw_logged = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_visible_logged = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_tx = tx.clone();
        let callback_partial = emitted_partial.clone();
        let callback_first_raw = first_raw_logged.clone();
        let callback_first_visible = first_visible_logged.clone();
        let provider_for_callback = provider.clone();
        let callback_trace_id = trace_id.clone();
        let native_tool_calls: NativeToolCalls = Arc::new(Mutex::new(HashMap::new()));
        let callback_native_tool_calls = native_tool_calls.clone();
        let callback_stream_redactor = stream_redactor.clone();
        let session_metadata: Arc<Mutex<Option<ProviderSessionMetadata>>> =
            Arc::new(Mutex::new(None));
        let callback_session_metadata = session_metadata.clone();
        let on_output: OnEventCallback = Arc::new(move |line: String| {
            let tx = callback_tx.clone();
            let provider = provider_for_callback.clone();
            let emitted_partial = callback_partial.clone();
            let first_raw_logged = callback_first_raw.clone();
            let first_visible_logged = callback_first_visible.clone();
            let native_tool_calls = callback_native_tool_calls.clone();
            let stream_redactor = callback_stream_redactor.clone();
            let trace_id = callback_trace_id.clone();
            let session_metadata = callback_session_metadata.clone();
            Box::pin(async move {
                if !first_raw_logged.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::info!(
                    component = "ai_turn_timing",
                                turn_id = %trace_id,
                                provider = %provider.name(),
                                phase = "harness_first_raw_output",
                                total_ms = timing_started.elapsed().as_millis() as u64,
                                "AI turn timing"
                            );
                }
                let native_deltas =
                    native_tool_deltas(provider.as_ref(), &line, &native_tool_calls);
                if let Some(metadata) = extract_session_metadata(provider.name(), &line) {
                    merge_session_metadata(&session_metadata, metadata);
                }
                let mut emitted_visible = !native_deltas.is_empty();
                for delta in native_deltas {
                    if tx.send(Ok(delta)).await.is_err() {
                        return;
                    }
                }
                if let Some(text) = provider.extract_partial_text(&line) {
                    emitted_partial.store(true, std::sync::atomic::Ordering::Relaxed);
                    let text = match stream_redactor.lock() {
                        Ok(mut redactor) => redactor.push(&text),
                        Err(poisoned) => poisoned.into_inner().push(&text),
                    };
                    if !text.is_empty() {
                        emitted_visible = true;
                        let _ = tx.send(Ok(ChatStreamDelta::Text(text))).await;
                    }
                } else if !emitted_partial.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(text) = provider.extract_assistant_text(&line) {
                        let text = match stream_redactor.lock() {
                            Ok(mut redactor) => redactor.push(&text),
                            Err(poisoned) => poisoned.into_inner().push(&text),
                        };
                        if !text.is_empty() {
                            emitted_visible = true;
                            let _ = tx.send(Ok(ChatStreamDelta::Text(text))).await;
                        }
                    }
                }
                if emitted_visible
                    && !first_visible_logged.swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    tracing::info!(
                    component = "ai_turn_timing",
                                turn_id = %trace_id,
                                provider = %provider.name(),
                                phase = "harness_first_visible_delta",
                                total_ms = timing_started.elapsed().as_millis() as u64,
                                "AI turn timing"
                            );
                }
            })
        });
        let tx_for_error = tx.clone();
        drop(tx);
        let task_trace_id = trace_id.clone();
        let sandbox_handles = self.sandbox_handles.clone();
        let sandbox_label = workspace.sandbox_label.clone();
        let capture_session_title = request.capture_session_title;
        let task = tokio::spawn(async move {
            let _model_relay_guard = model_relay_guard;
            let mut stop_on_drop = StopSandboxOnDrop {
                armed: true,
                provider: sandbox.clone(),
                handle: handle.clone(),
                handles: sandbox_handles,
                sandbox_label,
                sandbox_slot,
                global_permit: Some(permit),
                sandbox_permit: Some(sandbox_permit),
            };
            tracing::info!(
            component = "ai_turn_timing",
                turn_id = %task_trace_id,
                provider = %provider.name(),
                phase = "harness_exec_started",
                total_ms = timing_started.elapsed().as_millis() as u64,
                "AI turn timing"
            );
            let result = tokio::time::timeout(
                timeout,
                sandbox.exec(&handle, command, sandbox_env, Some(on_output)),
            )
            .await;
            let redactor_tail = match stream_redactor.lock() {
                Ok(mut redactor) => redactor.finish(),
                Err(poisoned) => poisoned.into_inner().finish(),
            };
            if !redactor_tail.is_empty() {
                let _ = tx_for_error
                    .send(Ok(ChatStreamDelta::Text(redactor_tail)))
                    .await;
            }
            let timed_out = result.is_err();
            let outcome = match &result {
                Ok(Ok(output)) if output.exit_code == 0 => "success",
                Ok(Ok(_)) => "nonzero_exit",
                Ok(Err(_)) => "sandbox_error",
                Err(_) => "timeout",
            };
            tracing::info!(
            component = "ai_turn_timing",
                turn_id = %task_trace_id,
                provider = %provider.name(),
                phase = "harness_process_complete",
                outcome,
                total_ms = timing_started.elapsed().as_millis() as u64,
                "AI turn timing"
            );
            let mut provider_session_metadata = session_metadata
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if capture_session_title {
                if let Some(metadata) = provider_session_metadata.as_mut() {
                    if metadata.title.is_none() {
                        if let Some(session_id) = metadata.session_id.as_deref() {
                            metadata.title = resolve_sandbox_session_title(
                                provider.name(),
                                sandbox.as_ref(),
                                &handle,
                                session_id,
                            )
                            .await;
                        }
                    }
                }
            }
            if let Some(metadata) = provider_session_metadata {
                let _ = tx_for_error
                    .send(Ok(ChatStreamDelta::SessionMetadata {
                        session_id: metadata.session_id,
                        title: metadata.title,
                    }))
                    .await;
            }
            if !timed_out {
                if let Some(secret_path) = mcp_secret_path.as_ref() {
                    match sandbox
                        .exec_as_root(
                            &handle,
                            sandbox_mcp_cleanup_command(secret_path),
                            HashMap::new(),
                            None,
                        )
                        .await
                    {
                        Ok(output) if output.exit_code == 0 => {}
                        Ok(output) => tracing::warn!(
                            provider = %provider.name(),
                            exit_code = output.exit_code,
                            stderr = %scrub_and_bound(&output.stderr),
                            "failed to remove expired sandbox MCP capability file"
                        ),
                        Err(error) => tracing::warn!(
                            provider = %provider.name(),
                            error = %error,
                            "failed to remove expired sandbox MCP capability file"
                        ),
                    }
                }
            }
            if timed_out {
                stop_on_drop.stop_now().await;
            } else {
                stop_on_drop.disarm();
            }
            let error = match result {
                Ok(Ok(output)) if output.exit_code == 0 => None,
                Ok(Ok(output)) => Some(AiError::Provider {
                    purpose,
                    reason: scrub_and_bound(&format!(
                        "sandboxed {} exited with code {}: {}",
                        provider.name(),
                        output.exit_code,
                        if output.stderr.trim().is_empty() {
                            output.stdout
                        } else {
                            output.stderr
                        }
                    )),
                }),
                Ok(Err(error)) => Some(map_agent_error("chat.application.sandbox", error)),
                Err(_) => Some(AiError::Provider {
                    purpose,
                    reason: format!("sandboxed harness timed out after {}s", timeout.as_secs()),
                }),
            };
            if let Some(error) = error {
                let _ = tx_for_error.send(Err(error)).await;
            }
        });
        let stream = futures::stream::unfold(
            (rx, AbortTaskOnDrop(task)),
            |(mut receiver, abort_on_drop)| async move {
                receiver
                    .recv()
                    .await
                    .map(|item| (item, (receiver, abort_on_drop)))
            },
        );
        Ok(Box::pin(stream))
    }
}

/// Delete only the exact, unguessable turn capability path generated by
/// [`configure_sandbox_mcp`]. The secrets directory is deliberately `0710`:
/// the harness can traverse to its own file but cannot list or unlink files.
/// Cleanup therefore runs through the provider's root execution boundary.
fn sandbox_mcp_cleanup_command(secret_path: &str) -> Vec<String> {
    vec![
        "rm".to_string(),
        "-f".to_string(),
        "--".to_string(),
        secret_path.to_string(),
    ]
}

fn is_child_path(root: &Path, path: &Path) -> bool {
    path.is_absolute() && path.starts_with(root) && path != root
}

fn configure_sandbox_model_relay(
    provider: &str,
    environment: &mut HashMap<String, String>,
    relay: &SandboxModelRelay,
) -> Result<(), AiError> {
    match provider {
        "claude_cli" => {
            environment.insert("ANTHROPIC_BASE_URL".to_string(), relay.base_url.clone());
            environment.insert("ANTHROPIC_AUTH_TOKEN".to_string(), relay.bearer.clone());
            environment.insert(
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
                "1".to_string(),
            );
            environment.insert("DISABLE_TELEMETRY".to_string(), "1".to_string());
            environment.insert("DISABLE_ERROR_REPORTING".to_string(), "1".to_string());
            environment.insert("DISABLE_BUG_COMMAND".to_string(), "1".to_string());
            Ok(())
        }
        other => Err(AiError::Provider {
            purpose: "chat.application.model_relay".to_string(),
            reason: format!("sandbox model relay is not implemented for '{other}'"),
        }),
    }
}

/// Register the same turn-scoped platform MCP server with each supported
/// sandbox harness. Only Claude needs a file; it lives on the sandbox tmpfs
/// and is removed when the provider process finishes. Codex/OpenCode receive
/// the bearer only in their process environment. In every case the value is a
/// narrow capability, not a reusable user/API token.
fn configure_sandbox_mcp(
    provider: &str,
    command: &mut Vec<String>,
    environment: &mut HashMap<String, String>,
    secret_files: &mut Vec<(String, Vec<u8>)>,
    server: Option<&temps_ai::HarnessMcpServer>,
) -> Result<Option<String>, AiError> {
    let Some(server) = server else {
        return Ok(None);
    };
    match provider {
        "claude_cli" => {
            let secret_path = format!(
                "/run/secrets/temps-chat-mcp-{}.json",
                uuid::Uuid::new_v4().simple()
            );
            let contents = serde_json::to_vec(&serde_json::json!({
                "mcpServers": {
                    "temps-chat": {
                        "type": "http",
                        "url": server.url,
                        "headers": {
                            "Authorization": format!("Bearer {}", server.authorization_token),
                        }
                    }
                }
            }))
            .map_err(|error| AiError::Provider {
                purpose: "chat.application.mcp".to_string(),
                reason: format!("could not encode sandbox MCP configuration: {error}"),
            })?;
            secret_files.push((secret_path.clone(), contents));
            command.extend([
                "--mcp-config".to_string(),
                secret_path.clone(),
                "--allowedTools".to_string(),
                // Claude documents the bare server name as the way to allow
                // every tool on that server; glob patterns are not supported.
                // Platform tools remain scoped by the one-turn bearer and
                // perform their own server-side authorization.
                "mcp__temps-chat".to_string(),
                "--permission-prompt-tool".to_string(),
                "mcp__temps-chat__temps_native_permission".to_string(),
            ]);
            Ok(Some(secret_path))
        }
        "codex_cli" => {
            command.extend([
                "--config".to_string(),
                format!("mcp_servers.temps_chat.url=\"{}\"", server.url),
                "--config".to_string(),
                "mcp_servers.temps_chat.bearer_token_env_var=\"TEMPS_CHAT_MCP_TOKEN\"".to_string(),
            ]);
            environment.insert(
                "TEMPS_CHAT_MCP_TOKEN".to_string(),
                server.authorization_token.clone(),
            );
            Ok(None)
        }
        "opencode" => {
            environment.insert(
                "OPENCODE_CONFIG_CONTENT".to_string(),
                serde_json::json!({
                    "mcp": {
                        "temps-chat": {
                            "type": "remote",
                            "url": server.url,
                            "headers": {
                                "Authorization": format!("Bearer {}", server.authorization_token),
                            }
                        }
                    },
                    "permission": {"temps_chat_*": "allow"}
                })
                .to_string(),
            );
            Ok(None)
        }
        other => Err(AiError::Provider {
            purpose: "chat.application.mcp".to_string(),
            reason: format!("sandbox MCP configuration is not implemented for '{other}'"),
        }),
    }
}

fn sandbox_harness_command(
    provider: &str,
    prompt: &str,
    request: &ChatTurnRequest,
) -> Result<Vec<String>, AiError> {
    let mut command = match provider {
        "claude_cli" => {
            let mut command = vec!["claude".to_string(), "--print".to_string()];
            if let Some(session_id) = request.resume_session_id.as_deref() {
                command.push("--resume".to_string());
                command.push(session_id.to_string());
            }
            command.extend([
                prompt.to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--verbose".to_string(),
                "--include-partial-messages".to_string(),
            ]);
            command
        }
        "codex_cli" => {
            let mut command = vec!["codex".to_string(), "exec".to_string()];
            if let Some(session_id) = request.resume_session_id.as_deref() {
                command.push("resume".to_string());
                command.push(session_id.to_string());
            }
            command.extend([
                prompt.to_string(),
                "--json".to_string(),
                "--skip-git-repo-check".to_string(),
            ]);
            command
        }
        "opencode" => {
            let mut command = vec![
                "opencode".to_string(),
                "run".to_string(),
                "--format".to_string(),
                "json".to_string(),
            ];
            if let Some(session_id) = request.resume_session_id.as_deref() {
                command.push("--session".to_string());
                command.push(session_id.to_string());
            }
            command
        }
        _ => {
            return Err(AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!("sandbox execution is not implemented for harness '{provider}'"),
            })
        }
    };
    if provider != "opencode" {
        if let Some(model) = request.model.as_deref().filter(|model| !model.is_empty()) {
            // All registered harnesses accept an explicit model flag. Claude's
            // parser accepts it after --print's prompt (matching its native CLI).
            command.push("--model".to_string());
            command.push(model.to_string());
        }
    }
    apply_sandbox_runtime_options(&mut command, provider, request)?;
    if provider == "opencode" {
        if let Some(model) = request.model.as_deref().filter(|model| !model.is_empty()) {
            command.push("--model".to_string());
            command.push(model.to_string());
        }
        command.push(prompt.to_string());
    }
    Ok(command)
}

/// Keep sandbox harness invocations honest about the controls shown in the
/// composer.  The Docker/Firecracker sandbox remains the outer boundary, but
/// it must not silently ignore the model's own effort or permission policy.
///
/// These flags mirror the provider adapters' normal `AiRunConfig` mappings.
/// We keep the mapping here because sandbox execution deliberately avoids the
/// host subprocess and its ambient configuration.
fn apply_sandbox_runtime_options(
    command: &mut Vec<String>,
    provider: &str,
    request: &ChatTurnRequest,
) -> Result<(), AiError> {
    match provider {
        "claude_cli" => {
            let permission = match request.permission_mode.as_deref() {
                Some("full-access") => "bypassPermissions",
                Some("accept-edits") => "acceptEdits",
                Some("plan") => "plan",
                _ => "default",
            };
            command.push("--permission-mode".to_string());
            command.push(permission.to_string());

            match request.thinking_level.as_deref() {
                None | Some("default") => {}
                Some("off") => {
                    command.push("--effort".to_string());
                    command.push("high".to_string());
                    command.push("--settings".to_string());
                    command.push(r#"{\"alwaysThinkingEnabled\":false}"#.to_string());
                }
                Some("ultracode") => {
                    command.push("--effort".to_string());
                    command.push("xhigh".to_string());
                    command.push("--settings".to_string());
                    command.push(r#"{\"ultracode\":true}"#.to_string());
                }
                Some(effort) => {
                    command.push("--effort".to_string());
                    command.push(effort.to_string());
                }
            }
        }
        "codex_cli" => {
            match request.permission_mode.as_deref() {
                Some("full-access") => {
                    command.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                }
                Some("auto-review") | Some("auto") | None => {
                    command.extend([
                        "--sandbox".to_string(),
                        "workspace-write".to_string(),
                        "--config".to_string(),
                        "approval_policy=\"on-request\"".to_string(),
                        "--config".to_string(),
                        "approvals_reviewer=\"auto_review\"".to_string(),
                    ]);
                }
                Some(mode) => {
                    return Err(AiError::Provider {
                        purpose: request.purpose.clone(),
                        reason: format!("unsupported Codex sandbox permission mode '{mode}'"),
                    });
                }
            }
            if let Some(effort) = request
                .thinking_level
                .as_deref()
                .filter(|effort| *effort != "default")
            {
                command.push("--config".to_string());
                command.push(format!("model_reasoning_effort=\"{effort}\""));
            }
        }
        "opencode" => {
            // OpenCode calls its permission selection an agent and its
            // reasoning selection a variant. Both precede the positional
            // prompt, which `sandbox_harness_command` appends afterwards.
            if let Some(agent) = request.permission_mode.as_deref() {
                command.push("--agent".to_string());
                command.push(agent.to_string());
            }
            if let Some(variant) = request
                .thinking_level
                .as_deref()
                .filter(|variant| *variant != "default")
            {
                command.push("--variant".to_string());
                command.push(variant.to_string());
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Prompt construction helpers
// ---------------------------------------------------------------------------

/// Compose an [`AiRequest`] into a flat text prompt. When a system instruction
/// is present it is prepended with a `[System]` header so CLI models that lack
/// a native system-prompt channel still receive the full context.
fn build_prompt(request: &AiRequest) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(system) = &request.system {
        let s = system.trim();
        if !s.is_empty() {
            parts.push(format!("[System]\n{}", s));
        }
    }
    parts.push(request.prompt.clone());
    parts.join("\n\n")
}

/// Concatenate a `ChatTurnRequest`'s message history into a flat prompt,
/// suitable for passing to a CLI that has no native multi-turn API.
fn build_chat_prompt(request: &ChatTurnRequest) -> String {
    request
        .messages
        .iter()
        .filter(|m| !m.content.is_empty())
        .map(|m| format!("[{}]\n{}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Build the prompt sent to a sandbox harness. A native resumed session already
/// owns the preceding provider transcript, so replaying the full database history
/// would duplicate every prior turn and eventually exceed the process argument
/// limit. The database history is still supplied on the request as the durable
/// recovery source; a resumed invocation sends only the newest user continuation.
fn build_sandbox_chat_prompt(request: &ChatTurnRequest) -> String {
    if request.resume_session_id.is_none() {
        return build_chat_prompt(request);
    }

    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| message.content.clone())
        .unwrap_or_else(|| build_chat_prompt(request))
}

/// Extract the human-readable assistant reply from a CLI run's raw NDJSON
/// stdout — every provider here is invoked with a `--output-format
/// stream-json`/`--json`/`--format json` flag, so `raw_output` is a stream
/// of protocol events (hooks, tool calls, rate-limit frames, a final result
/// summary), not plain prose. Forwarding it unfiltered means the caller
/// (the chat UI, a `complete()` consumer) displays the wire protocol instead
/// of the answer.
///
/// Concatenates every line's extracted text (via
/// [`temps_agents::ai_cli::extract_assistant_text`], dispatched by
/// `provider_name`) and falls back to the raw trimmed output only when no
/// line yielded anything — a defensive net for a provider/output mode that
/// isn't JSON at all, so a real answer is never dropped just because
/// parsing found nothing to extract.
fn extract_final_text(provider_name: &str, raw_output: &str) -> String {
    let mut extracted = String::new();
    for line in raw_output.lines() {
        if let Some(text) = temps_agents::ai_cli::extract_assistant_text(provider_name, line) {
            extracted.push_str(&text);
        } else if let Some(tool_name) =
            temps_agents::ai_cli::dropped_tool_use_name(provider_name, line)
        {
            // ADR-038: see the matching log point in chat_stream's on_event —
            // same rationale, `complete()`'s one-shot path hits it too.
            tracing::warn!(
                provider = %provider_name,
                tool_name = %tool_name,
                "agent CLI attempted a tool call that CLI-chat cannot bridge back to the user; dropping (see ADR-038)"
            );
        }
    }
    if extracted.is_empty() {
        raw_output.trim().to_owned()
    } else {
        extracted
    }
}

/// Map an [`AgentError`] to an [`AiError::Provider`] with the request's
/// purpose tag and a descriptive reason.
///
/// Defensively re-scrubs the error text through [`scrub_and_bound`] before it
/// reaches `AiError::Provider.reason` (which callers may surface to end users
/// or ship to logs). Today every `AgentError::AiCliFailed` already passes
/// through `summarize_cli_failure` (which scrubs) before reaching here, but
/// this function accepts any `AgentError` — a future provider or error path
/// that skips that upstream scrub must not be able to leak a credential
/// pattern through this boundary.
fn map_agent_error(purpose: &str, err: AgentError) -> AiError {
    AiError::Provider {
        purpose: purpose.to_owned(),
        reason: scrub_and_bound(&err.to_string()),
    }
}

/// Reject prompts over [`MAX_PROMPT_BYTES`] before any resource (semaphore
/// permit, tempdir, subprocess) is acquired for them.
fn check_prompt_size(purpose: &str, prompt: &str) -> Result<(), AiError> {
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(AiError::Provider {
            purpose: purpose.to_owned(),
            reason: format!(
                "prompt exceeds maximum size ({} bytes > {MAX_PROMPT_BYTES} byte limit)",
                prompt.len()
            ),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AiService implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl AiService for AgentCliAiService {
    /// Returns `true` when the underlying CLI reports both `installed` and
    /// `authenticated`. Callers should gate prompt construction on this check.
    async fn is_available(&self) -> bool {
        let status =
            get_status_cached(self.provider.as_ref(), false, PROVIDER_STATUS_TIMEOUT).await;
        if status
            .as_ref()
            .is_some_and(|status| status.installed && status.authenticated)
        {
            return true;
        }
        // Application harnesses run in the Temps sandbox, not in the server
        // process. A host `claude setup-token` / `codex login` is therefore
        // neither required nor consulted for this route. The credential itself
        // remains checked and decrypted only immediately before the sandbox
        // turn, so this availability probe never reads secret material.
        self.provider.name() == "claude_cli"
            && self.sandbox_provider.is_some()
            && self.sandbox_credentials.is_some()
            && self.sandbox_model_relay.is_some()
    }

    async fn capabilities_for(
        &self,
        _provider: Option<&str>,
        refresh: temps_ai::RefreshPolicy,
    ) -> Result<temps_ai::ProviderCapabilities, AiError> {
        let refresh_live = refresh == temps_ai::RefreshPolicy::Refresh;
        let status = get_status_cached(
            self.provider.as_ref(),
            refresh_live,
            PROVIDER_STATUS_TIMEOUT,
        )
        .await;
        let host_ready = status
            .as_ref()
            .is_some_and(|status| status.installed && status.authenticated);
        if !host_ready
            && (self.provider.name() != "claude_cli"
                || self.sandbox_provider.is_none()
                || self.sandbox_credentials.is_none()
                || self.sandbox_model_relay.is_none())
        {
            return Err(AiError::NotAvailable);
        }
        let identity = status
            .as_ref()
            .map(|status| {
                format!(
                    "{}|{}|{}|{}",
                    status.version.as_deref().unwrap_or("unknown"),
                    status.auth_method.as_deref().unwrap_or("unknown"),
                    status.email.as_deref().unwrap_or("unknown"),
                    status.subscription_type.as_deref().unwrap_or("unknown")
                )
            })
            .unwrap_or_else(|| "unknown|unknown|unknown|unknown".to_string());
        let models = if refresh_live {
            discover_model_capabilities_cached(self.provider.as_ref(), identity, true)
                .await
                .models
        } else {
            cached_model_capabilities(self.provider.name(), &identity)
                .await
                .map(|snapshot| snapshot.models)
                .unwrap_or_default()
        };
        provider_capabilities_from_models(self.provider.name(), models).ok_or_else(|| {
            AiError::Provider {
                purpose: "provider.capabilities".to_string(),
                reason: format!(
                    "provider '{}' has no registered capability contract",
                    self.provider.name()
                ),
            }
        })
    }

    /// CLI chat exposes scoped tools through a per-turn loopback MCP bridge.
    async fn chat_capable(&self) -> bool {
        self.is_available().await
    }

    /// Single-pass completion through the agent CLI.
    ///
    /// Acquires one semaphore permit (non-blocking) before starting the
    /// subprocess. The CLI executes in a throwaway `tempdir` so it has no
    /// access to project files. JSON is extracted from the output on a
    /// best-effort basis (useful for [`temps_ai::complete_typed`] callers;
    /// note that no `response_format` enforcement is possible with CLI
    /// providers — see ADR-037 Consequences).
    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
        let purpose = request.purpose.clone();
        let prompt = build_prompt(&request);
        check_prompt_size(&purpose, &prompt)?;

        let _permit = Arc::clone(&self.concurrency)
            .try_acquire_owned()
            .map_err(|_| AiError::Provider {
                purpose: purpose.clone(),
                reason: "agent CLI concurrency limit reached — try again shortly".into(),
            })?;

        let run_dir = tempfile::tempdir_in(&self.scratch_dir).map_err(|e| {
            tracing::error!(
                purpose = %purpose,
                scratch_dir = %self.scratch_dir.display(),
                error = %e,
                "failed to create agent CLI scratch tempdir"
            );
            AiError::Provider {
                purpose: purpose.clone(),
                reason: "scratch directory unavailable; contact your administrator".into(),
            }
        })?;

        let cfg = AiRunConfig {
            work_dir: run_dir.path().to_owned(),
            prompt,
            api_key: String::new(), // subscription mode — ambient credential
            max_turns: 1,
            timeout: self.timeout,
            model: request.model.clone(),
            thinking_level: None,
            permission_mode: None,
            on_event: None, // single-pass; no streaming overhead needed
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: None,
        };

        let result = tokio::time::timeout(self.timeout, self.provider.run(cfg))
            .await
            .map_err(|_| AiError::Provider {
                purpose: purpose.clone(),
                reason: format!("CLI timed out after {}s", self.timeout.as_secs()),
            })?
            .map_err(|e| map_agent_error(&purpose, e))?;

        let text = extract_final_text(self.provider.name(), &result.output);
        let json = extract_json_block(&text);

        Ok(AiResponse {
            text,
            json,
            model: result.model.unwrap_or_default(),
        })
    }

    /// Tool-less streaming completion through the agent CLI.
    ///
    /// Returns [`AiError::NotAvailable`] immediately when `request.tools` is
    /// non-empty. Agent CLIs cannot be fed an external function-calling
    /// protocol; tool-calling workloads must continue to route through the
    /// gateway (ADR-037 Decision §1).
    ///
    /// For tool-less requests each line emitted by the CLI via its `on_event`
    /// callback is forwarded as a stream chunk. The semaphore permit is held
    /// for the lifetime of the CLI subprocess (the spawned task), not just
    /// until the stream consumer is dropped.
    async fn chat_stream(&self, request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        if !request.tools.is_empty() {
            return Err(AiError::NotAvailable);
        }

        let purpose = request.purpose.clone();
        let prompt = build_chat_prompt(&request);
        check_prompt_size(&purpose, &prompt)?;

        let permit = Arc::clone(&self.concurrency)
            .try_acquire_owned()
            .map_err(|_| AiError::Provider {
                purpose: purpose.clone(),
                reason: "agent CLI concurrency limit reached — try again shortly".into(),
            })?;

        let run_dir = tempfile::tempdir_in(&self.scratch_dir).map_err(|e| {
            tracing::error!(
                purpose = %purpose,
                scratch_dir = %self.scratch_dir.display(),
                error = %e,
                "failed to create agent CLI scratch tempdir"
            );
            AiError::Provider {
                purpose: purpose.clone(),
                reason: "scratch directory unavailable; contact your administrator".into(),
            }
        })?;

        // Channel capacity 64 provides enough buffer for a burst of lines
        // without back-pressure stalling the CLI subprocess.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, AiError>>(64);

        // Build on_event: clone tx so the original can be dropped immediately,
        // making on_event the sole remaining sender. When provider.run()
        // finishes and on_event is dropped, the channel closes automatically.
        //
        // Each raw line is the CLI's NDJSON wire protocol, not plain text
        // (see `extract_final_text`'s doc comment) — only forward what
        // `extract_partial_text`/`extract_assistant_text` recognize as
        // user-facing prose; every other event (hooks, tool calls,
        // rate-limit frames, the terminal result summary) is dropped rather
        // than shown to the user.
        let provider_name = self.provider.name().to_string();
        let tx_for_event = tx.clone();
        // Set once this turn has forwarded at least one incremental delta
        // (claude_cli only, today — see `extract_partial_text`). When set,
        // the later consolidated `assistant` event repeats that same text in
        // one shot, so it's skipped rather than forwarded as a duplicate. A
        // provider with no delta support (codex_cli, opencode) never sets
        // this, so its final consolidated text is still forwarded exactly as
        // before.
        let streamed_partial = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let on_event: OnEventCallback = Arc::new(move |line: String| {
            let tx = tx_for_event.clone();
            let provider_name = provider_name.clone();
            let streamed_partial = streamed_partial.clone();
            Box::pin(async move {
                if let Some(delta) =
                    temps_agents::ai_cli::extract_partial_text(&provider_name, &line)
                {
                    streamed_partial.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = tx.send(Ok(delta)).await;
                } else if let (false, Some(text)) = (
                    streamed_partial.load(std::sync::atomic::Ordering::Relaxed),
                    temps_agents::ai_cli::extract_assistant_text(&provider_name, &line),
                ) {
                    let _ = tx.send(Ok(text)).await;
                } else if let Some(tool_name) =
                    temps_agents::ai_cli::dropped_tool_use_name(&provider_name, &line)
                {
                    // ADR-038: CLI-chat has no channel to bridge this back to
                    // the user (no open stdin, no permission UI) — the turn
                    // silently produces less text than the model intended.
                    // Logging the tool name (never its input) makes that
                    // diagnosable from server logs instead of vanishing.
                    tracing::warn!(
                        provider = %provider_name,
                        tool_name = %tool_name,
                        "agent CLI attempted a tool call that CLI-chat cannot bridge back to the user; dropping (see ADR-038)"
                    );
                }
            })
        });
        let work_dir = run_dir.path().to_owned();
        let cfg = AiRunConfig {
            work_dir,
            prompt,
            api_key: String::new(),
            max_turns: 1,
            timeout: self.timeout,
            model: request.model.clone(),
            thinking_level: request.thinking_level.clone(),
            permission_mode: request.permission_mode.clone(),
            on_event: Some(on_event),
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: None,
        };

        let timeout = self.timeout;
        let provider = self.provider.clone();
        let tx_for_error = tx.clone();
        drop(tx);

        // Spawn the CLI subprocess. The permit is moved into this task so it
        // is held for the full CLI lifetime, not just while the stream consumer
        // is alive. When the CLI finishes (or times out), `on_event` is
        // dropped, closing the channel and terminating the stream.
        let task = tokio::spawn(async move {
            let _permit = permit;
            let _tempdir = run_dir; // keep tempdir alive for the run
            let result = tokio::time::timeout(timeout, provider.run(cfg)).await;
            let error = match result {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(map_agent_error(&purpose, error)),
                Err(_) => Some(AiError::Provider {
                    purpose,
                    reason: format!("CLI timed out after {}s", timeout.as_secs()),
                }),
            };
            if let Some(error) = error {
                let _ = tx_for_error.send(Err(error)).await;
            }
        });

        // Wrap the receiver as a TokenStream using unfold. This is Send
        // because Receiver<String>: Send.
        let stream = futures::stream::unfold(
            (rx, AbortTaskOnDrop(task)),
            |(mut receiver, abort_on_drop)| async move {
                receiver
                    .recv()
                    .await
                    .map(|item| (item, (receiver, abort_on_drop)))
            },
        );

        Ok(Box::pin(stream))
    }

    // chat() is intentionally NOT overridden: it defaults to
    // Err(AiError::NotAvailable), which is exactly right — agent CLIs have no
    // non-streaming function-calling path.

    /// Fails closed because this entry point cannot carry the conversation's
    /// scoped tool executor. Callers that need CLI context tools must use
    /// [`Self::chat_stream_turn_with_executor`], which exposes only the tools
    /// authorized for that turn through the authenticated loopback MCP bridge.
    async fn chat_stream_turn(&self, _request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream_turn_with_executor(
        &self,
        request: ChatTurnRequest,
        executor: Option<ToolExecutor>,
    ) -> Result<ChatTurnStream, AiError> {
        self.chat_stream_turn_with_services(
            request,
            TurnServices {
                tools: executor,
                interactions: None,
            },
        )
        .await
    }

    async fn chat_stream_turn_with_services(
        &self,
        request: ChatTurnRequest,
        services: TurnServices,
    ) -> Result<ChatTurnStream, AiError> {
        use futures::StreamExt;

        // Application conversations set this only after deriving a path under
        // TEMPS_DATA_DIR. That is the hard routing boundary: a harness turn
        // can never reach the legacy host scratch-directory implementation.
        if request.harness_workspace.is_some() {
            return self.sandbox_chat_stream_turn(request).await;
        }

        if request.tools.is_empty() && services.interactions.is_none() {
            let stream = self.chat_stream(request).await?;
            return Ok(Box::pin(stream.map(|item| item.map(ChatStreamDelta::Text))));
        }
        let prompt = build_chat_prompt(&request);
        check_prompt_size(&request.purpose, &prompt)?;
        let permit = Arc::clone(&self.concurrency)
            .try_acquire_owned()
            .map_err(|_| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "agent CLI concurrency limit reached — try again shortly".to_string(),
            })?;
        let run_dir =
            tempfile::tempdir_in(&self.scratch_dir).map_err(|error| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!("failed to create isolated CLI chat directory: {error}"),
            })?;

        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut mcp_bridge, bridge_event_task, mcp_config) = if request.tools.is_empty() {
            (None, None, None)
        } else {
            let executor = services.tools.clone().ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "scoped tool executor is required for CLI chat".to_string(),
            })?;
            let mut bridge = ScopedMcpBridge::start(request.tools.clone(), executor).await?;
            let config = bridge.config.clone();
            let mut mcp_events = bridge.take_events();
            let bridge_events_tx = events_tx.clone();
            let event_task = tokio::spawn(async move {
                while let Some(event) = mcp_events.recv().await {
                    if bridge_events_tx.send(event).is_err() {
                        break;
                    }
                }
            });
            (Some(bridge), Some(event_task), Some(config))
        };

        let provider_name = self.provider.name().to_string();
        let streamed_partial = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_tx = events_tx.clone();
        let native_tool_calls: NativeToolCalls = Arc::new(Mutex::new(HashMap::new()));
        let callback_native_tool_calls = native_tool_calls.clone();
        let provider_for_callback = self.provider.clone();
        let on_event: OnEventCallback = Arc::new(move |line: String| {
            let provider_name = provider_name.clone();
            let callback_tx = callback_tx.clone();
            let streamed_partial = streamed_partial.clone();
            let native_tool_calls = callback_native_tool_calls.clone();
            let provider = provider_for_callback.clone();
            Box::pin(async move {
                for delta in native_tool_deltas(provider.as_ref(), &line, &native_tool_calls) {
                    if callback_tx.send(Ok(delta)).is_err() {
                        return;
                    }
                }
                if let Some(text) =
                    temps_agents::ai_cli::extract_partial_text(&provider_name, &line)
                {
                    streamed_partial.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = callback_tx.send(Ok(ChatStreamDelta::Text(text)));
                } else if !streamed_partial.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(text) =
                        temps_agents::ai_cli::extract_assistant_text(&provider_name, &line)
                    {
                        let _ = callback_tx.send(Ok(ChatStreamDelta::Text(text)));
                    }
                }
            })
        });

        let interaction_tasks = Arc::new(std::sync::Mutex::new(Vec::new()));
        let interaction_task_owner = InteractionTaskOwner(interaction_tasks.clone());
        let permission_bridge = services.interactions.map(|interactions| {
            let permission_events = events_tx.clone();
            let interaction_tasks = interaction_tasks.clone();
            Arc::new(PermissionBridge {
                on_permission_request: Arc::new(move |request| {
                    let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
                    // The common handler registers synchronously when called.
                    // Do that before publishing the event so an immediate UI
                    // response cannot race into a missing-permission error.
                    let decision = interactions(request.clone());
                    let _ = permission_events
                        .send(Ok(ChatStreamDelta::PermissionRequested(request.clone())));
                    let waiter = tokio::spawn(async move {
                        if let Ok(decision) = decision.await {
                            let _ = decision_tx.send(decision);
                        }
                    });
                    match interaction_tasks.lock() {
                        Ok(mut tasks) => tasks.push(waiter),
                        Err(poisoned) => poisoned.into_inner().push(waiter),
                    }
                    decision_rx
                }),
            })
        });

        let config = AiRunConfig {
            work_dir: run_dir.path().to_owned(),
            prompt,
            api_key: String::new(),
            max_turns: 0,
            timeout: self.timeout,
            model: request.model,
            thinking_level: request.thinking_level,
            permission_mode: request.permission_mode,
            on_event: Some(on_event),
            permission_bridge,
            resume_session_id: None,
            mcp_server: mcp_config,
        };
        let provider = self.provider.clone();
        let purpose = request.purpose;
        let timeout = self.timeout;
        let task = tokio::spawn(async move {
            let _interaction_tasks = interaction_task_owner;
            let _permit = permit;
            let _run_dir = run_dir;
            let outcome = tokio::time::timeout(timeout, provider.run_turn(config)).await;
            let error = match outcome {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(map_agent_error(&purpose, error)),
                Err(_) => Some(AiError::Provider {
                    purpose,
                    reason: format!("CLI timed out after {}s", timeout.as_secs()),
                }),
            };
            if let Some(error) = error {
                let _ = events_tx.send(Err(error));
            }
            if let Some(bridge) = mcp_bridge.take() {
                bridge.shutdown().await;
            }
            if let Some(event_task) = bridge_event_task {
                let _ = event_task.await;
            }
        });

        let stream = futures::stream::unfold(
            (events_rx, AbortTaskOnDrop(task)),
            |(mut receiver, abort_on_drop)| async move {
                receiver
                    .recv()
                    .await
                    .map(|event| (event, (receiver, abort_on_drop)))
            },
        );
        Ok(Box::pin(stream))
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use temps_agents::ai_cli::{invalidate_status_cache, AiCliStatus, AiRunResult};
    use temps_agents::error::AgentError;
    use temps_ai::streaming::ChatTool;
    use temps_ai::AiRequest;

    // -----------------------------------------------------------------------
    // Mock provider helpers
    // -----------------------------------------------------------------------

    fn available_status() -> AiCliStatus {
        AiCliStatus {
            provider: "mock".into(),
            installed: true,
            version: Some("1.0.0".into()),
            authenticated: true,
            auth_method: Some("oauth".into()),
            email: None,
            subscription_type: None,
            setup_hint: None,
        }
    }

    fn unavailable_status() -> AiCliStatus {
        AiCliStatus {
            provider: "mock".into(),
            installed: true,
            version: Some("1.0.0".into()),
            authenticated: false,
            auth_method: None,
            email: None,
            subscription_type: None,
            setup_hint: Some("Run: claude auth login".into()),
        }
    }

    #[test]
    fn sandbox_handle_cache_reuses_and_evicts_by_application_label() {
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: String::new(),
            model: None,
            called: Arc::new(AtomicBool::new(false)),
        });
        let scratch = tempfile::tempdir().expect("create agent CLI scratch directory");
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            2,
        );
        let handle = temps_agents::sandbox::SandboxHandle {
            sandbox_id: "sandbox-id".to_string(),
            sandbox_name: "temps-sandbox-app-one".to_string(),
            work_dir: PathBuf::from("/home/temps/workspace"),
            backend: temps_agents::sandbox::SandboxBackend::Docker,
            image: "test-image".to_string(),
        };

        service.cache_sandbox_handle("app-one".to_string(), handle);
        let cached = service
            .cached_sandbox_handle("app-one")
            .expect("cached application sandbox handle");
        assert_eq!(cached.sandbox_id, "sandbox-id");

        service.evict_sandbox_handle("app-one");
        assert!(service.cached_sandbox_handle("app-one").is_none());
    }

    #[tokio::test]
    async fn sandbox_slot_serializes_conversations_in_one_application() {
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: String::new(),
            model: None,
            called: Arc::new(AtomicBool::new(false)),
        });
        let scratch = tempfile::tempdir().expect("create agent CLI scratch directory");
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            2,
        );

        let first = service.sandbox_slot("app-one");
        let same_application = service.sandbox_slot("app-one");
        let other_application = service.sandbox_slot("app-two");
        let permit = first.try_acquire_owned().expect("first turn acquires slot");

        assert!(same_application.try_acquire_owned().is_err());
        assert!(other_application.try_acquire_owned().is_ok());
        drop(permit);
        assert!(service.sandbox_slot("app-one").try_acquire_owned().is_ok());
    }

    #[test]
    fn streaming_secret_redactor_masks_capabilities_split_across_deltas() {
        let mut redactor = StreamingSecretRedactor::new([
            "tmodel_livecap".to_string(),
            "tmcp_livecap".to_string(),
        ]);

        assert_eq!(redactor.push("model=tmod"), "model=");
        assert_eq!(redactor.push("el_live"), "");
        assert_eq!(redactor.push("cap platform=tm"), "[redacted] platform=");
        assert_eq!(
            redactor.push("cp_livecap response is intact"),
            "[redacted] response is intac"
        );
        assert_eq!(redactor.finish(), "t");
    }

    #[test]
    fn streaming_secret_redactor_flushes_non_secret_suffix() {
        let mut redactor = StreamingSecretRedactor::new(["tmodel_livecap".to_string()]);

        assert_eq!(redactor.push("answer tmod"), "answer ");
        assert_eq!(redactor.finish(), "tmod");
    }

    #[test]
    fn native_tool_deltas_pair_claude_bash_call_and_result() {
        let provider = temps_agents::ai_cli::claude::ClaudeCliProvider;
        let calls: NativeToolCalls = Arc::new(Mutex::new(HashMap::new()));
        let call_line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_bash","name":"Bash","input":{"command":"pwd"}}]}}"#;
        let result_line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_bash","content":"/workspace"}]}}"#;

        let calls_delta = native_tool_deltas(&provider, call_line, &calls);
        assert!(matches!(
            calls_delta.as_slice(),
            [ChatStreamDelta::ToolCall(ToolCall { id, name, arguments })]
                if id == "toolu_bash" && name == "Bash" && arguments == r#"{"command":"pwd"}"#
        ));

        let result_delta = native_tool_deltas(&provider, result_line, &calls);
        assert!(matches!(
            result_delta.as_slice(),
            [ChatStreamDelta::ToolResult { call, result }]
                if call.id == "toolu_bash" && call.name == "Bash" && result == "/workspace"
        ));
    }

    fn fixed_result(output: &str, model: Option<&str>) -> AiRunResult {
        AiRunResult {
            output: output.into(),
            exit_code: 0,
            tokens_input: Some(10),
            tokens_output: Some(20),
            model: model.map(String::from),
            changed_files: None,
            session_id: None,
            is_max_turns_error: false,
        }
    }

    /// A mock that returns a fixed output string. Uses a shared AtomicBool so
    /// tests can assert whether run() was called without owning the mock.
    struct MockProvider {
        status: AiCliStatus,
        output: String,
        model: Option<String>,
        called: Arc<AtomicBool>,
    }

    #[async_trait]
    impl AiCliProvider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn check_installed(&self) -> bool {
            self.status.installed
        }
        async fn get_status(&self) -> AiCliStatus {
            self.status.clone()
        }
        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            self.called.store(true, Ordering::SeqCst);
            Ok(fixed_result(&self.output, self.model.as_deref()))
        }
        async fn continue_conversation(
            &self,
            config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            self.run(config).await
        }
    }

    /// A mock that sleeps for a long time, used to trigger the timeout path.
    struct SlowProvider;

    #[async_trait]
    impl AiCliProvider for SlowProvider {
        fn name(&self) -> &str {
            "slow"
        }
        async fn check_installed(&self) -> bool {
            true
        }
        async fn get_status(&self) -> AiCliStatus {
            available_status()
        }
        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(fixed_result("", None))
        }
        async fn continue_conversation(
            &self,
            config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            self.run(config).await
        }
    }

    struct CancellationProvider {
        started: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicBool>,
    }

    struct MarkDropped(Arc<AtomicBool>);

    impl Drop for MarkDropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl AiCliProvider for CancellationProvider {
        fn name(&self) -> &str {
            "cancellation-test"
        }

        async fn check_installed(&self) -> bool {
            true
        }

        async fn get_status(&self) -> AiCliStatus {
            available_status()
        }

        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            let _mark_dropped = MarkDropped(self.dropped.clone());
            self.started.notify_one();
            std::future::pending().await
        }

        async fn continue_conversation(
            &self,
            config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            self.run(config).await
        }
    }

    struct FailingProvider;

    struct InteractionProvider;

    #[async_trait]
    impl AiCliProvider for InteractionProvider {
        fn name(&self) -> &str {
            "claude_cli"
        }

        async fn check_installed(&self) -> bool {
            true
        }

        async fn get_status(&self) -> AiCliStatus {
            available_status()
        }

        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            Ok(fixed_result("", None))
        }

        async fn run_turn(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            let bridge = config.permission_bridge.expect("interaction bridge");
            let decision = (bridge.on_permission_request)(temps_ai::PermissionRequest {
                id: "permission-1".to_string(),
                kind: temps_ai::PermissionKind::ToolApproval,
                tool_name: "temps".to_string(),
                input: serde_json::json!({}),
            });
            let _ = decision.await;
            Ok(fixed_result("", None))
        }

        async fn continue_conversation(
            &self,
            config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            self.run(config).await
        }
    }

    #[async_trait]
    impl AiCliProvider for FailingProvider {
        fn name(&self) -> &str {
            "codex_cli"
        }
        async fn check_installed(&self) -> bool {
            true
        }
        async fn get_status(&self) -> AiCliStatus {
            available_status()
        }
        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            Err(AgentError::AiCliFailed {
                provider: "codex_cli".to_string(),
                exit_code: 1,
                stderr: "not inside a trusted directory".to_string(),
            })
        }
        async fn continue_conversation(
            &self,
            config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            self.run(config).await
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: complete() maps AiRunResult → AiResponse correctly
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_complete_maps_to_ai_response() {
        let called = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: "Hello, world!".into(),
            model: Some("claude-3-5-sonnet".into()),
            called: called.clone(),
        });

        let scratch = tempfile::tempdir().unwrap();
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            2,
        );

        let result = service
            .complete(AiRequest {
                purpose: "test.complete".into(),
                prompt: "Say hello".into(),
                ..Default::default()
            })
            .await;

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let response = result.unwrap();
        assert_eq!(response.text, "Hello, world!");
        assert_eq!(response.model, "claude-3-5-sonnet");
        assert!(
            called.load(Ordering::SeqCst),
            "provider.run() was not called"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: complete() extracts JSON from prose output
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_complete_extracts_json_from_output() {
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: r#"Here is the result: {"status": "ok", "count": 42}"#.into(),
            model: None,
            called: Arc::new(AtomicBool::new(false)),
        });

        let scratch = tempfile::tempdir().unwrap();
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            2,
        );

        let response = service
            .complete(AiRequest {
                purpose: "test.json".into(),
                prompt: "Return JSON".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(response.json.is_some(), "expected JSON to be extracted");
        assert_eq!(response.json.unwrap()["count"], 42);
    }

    // -----------------------------------------------------------------------
    // Test 3: provider timeout surfaces as AiError::Provider
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_complete_timeout_surfaces_as_provider_error() {
        let provider: Arc<dyn AiCliProvider> = Arc::new(SlowProvider);

        let scratch = tempfile::tempdir().unwrap();
        // 1ms timeout ensures the SlowProvider (10s sleep) always times out.
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_millis(1),
            2,
        );

        let result = service
            .complete(AiRequest {
                purpose: "test.timeout".into(),
                prompt: "This will time out".into(),
                ..Default::default()
            })
            .await;

        match &result {
            Err(AiError::Provider { purpose, reason }) => {
                assert_eq!(purpose, "test.timeout");
                assert!(
                    reason.contains("timed out"),
                    "expected 'timed out' in reason, got: {}",
                    reason
                );
            }
            other => panic!("expected AiError::Provider with timeout, got: {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Test 4: chat_stream() with non-empty tools → NotAvailable, no CLI call
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_chat_stream_with_tools_returns_not_available() {
        let called = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: "irrelevant".into(),
            model: None,
            called: called.clone(),
        });

        let scratch = tempfile::tempdir().unwrap();
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            2,
        );

        let request = ChatTurnRequest {
            purpose: "test.chat".into(),
            tools: vec![ChatTool {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({}),
            }],
            ..Default::default()
        };

        let result = service.chat_stream(request).await;

        assert!(
            matches!(result, Err(AiError::NotAvailable)),
            "expected NotAvailable for tool-bearing request, got: {:?}",
            result.err()
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "provider.run() must not be called when tools are present"
        );
    }

    #[tokio::test]
    async fn dropping_chat_stream_cancels_the_provider_turn() {
        let started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn AiCliProvider> = Arc::new(CancellationProvider {
            started: started.clone(),
            dropped: dropped.clone(),
        });
        let scratch = tempfile::tempdir().expect("scratch directory");
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            1,
        );
        let stream = service
            .chat_stream(ChatTurnRequest {
                purpose: "test.cancel".to_string(),
                ..Default::default()
            })
            .await
            .expect("stream starts");

        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("provider started");
        drop(stream);

        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider future cancelled when stream dropped");
    }

    #[tokio::test]
    async fn interaction_is_registered_before_its_stream_event_is_visible() {
        use futures::StreamExt;

        let provider: Arc<dyn AiCliProvider> = Arc::new(InteractionProvider);
        let scratch = tempfile::tempdir().expect("scratch directory");
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            1,
        );
        let registered = Arc::new(AtomicBool::new(false));
        let registered_for_interaction = registered.clone();
        let interactions: temps_ai::InteractionExecutor = Arc::new(move |_| {
            registered_for_interaction.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(temps_ai::PermissionDecision::AllowTool) })
        });
        let mut stream = service
            .chat_stream_turn_with_services(
                ChatTurnRequest {
                    purpose: "test.interaction-order".to_string(),
                    ..Default::default()
                },
                TurnServices {
                    tools: None,
                    interactions: Some(interactions),
                },
            )
            .await
            .expect("turn starts");

        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("interaction event arrives")
            .expect("stream item")
            .expect("interaction event succeeds");
        assert!(matches!(event, ChatStreamDelta::PermissionRequested(_)));
        assert!(
            registered.load(Ordering::SeqCst),
            "interaction must be registered before the UI can resolve it"
        );
    }

    #[tokio::test]
    async fn cancelling_turn_drops_pending_interaction_waiter() {
        use futures::StreamExt;

        let provider: Arc<dyn AiCliProvider> = Arc::new(InteractionProvider);
        let scratch = tempfile::tempdir().expect("scratch directory");
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            1,
        );
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_for_interaction = dropped.clone();
        let interactions: temps_ai::InteractionExecutor = Arc::new(move |_| {
            let marker = MarkDropped(dropped_for_interaction.clone());
            Box::pin(async move {
                let _marker = marker;
                std::future::pending().await
            })
        });
        let mut stream = service
            .chat_stream_turn_with_services(
                ChatTurnRequest {
                    purpose: "test.interaction-cancel".to_string(),
                    ..Default::default()
                },
                TurnServices {
                    tools: None,
                    interactions: Some(interactions),
                },
            )
            .await
            .expect("turn starts");

        tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("interaction event arrives");
        drop(stream);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("interaction waiter cancelled with turn");
    }

    #[tokio::test]
    async fn test_chat_stream_surfaces_cli_failure_to_consumer() {
        use futures::StreamExt;

        let scratch = tempfile::tempdir().unwrap();
        let service = AgentCliAiService::new(
            Arc::new(FailingProvider),
            scratch.path().to_owned(),
            Duration::from_secs(30),
            1,
        );
        let request = ChatTurnRequest {
            purpose: "test.codex_stream".into(),
            messages: vec![temps_ai::ChatMessage {
                role: "user".into(),
                content: "hello".into(),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..Default::default()
        };

        let mut stream = service.chat_stream(request).await.unwrap();
        let first = stream.next().await.expect("stream must report CLI failure");

        match first {
            Err(AiError::Provider { purpose, reason }) => {
                assert_eq!(purpose, "test.codex_stream");
                assert!(reason.contains("not inside a trusted directory"));
            }
            other => panic!("expected provider failure, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 5: is_available() reflects CLI status
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_is_available_reflects_status() {
        let scratch = tempfile::tempdir().unwrap();

        // installed + authenticated → true
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: String::new(),
            model: None,
            called: Arc::new(AtomicBool::new(false)),
        });
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            2,
        );
        assert!(
            service.is_available().await,
            "installed+authenticated should be available"
        );

        // The production status cache is process-wide because each provider
        // has one host authentication state. This test deliberately swaps in
        // a second provider implementation with the same id, so clear that
        // shared state before asserting the new status.
        invalidate_status_cache().await;

        // installed but NOT authenticated → false
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: unavailable_status(),
            output: String::new(),
            model: None,
            called: Arc::new(AtomicBool::new(false)),
        });
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            2,
        );
        assert!(
            !service.is_available().await,
            "unauthenticated should not be available"
        );
    }

    fn test_mcp_state() -> (
        McpBridgeState,
        tokio::sync::mpsc::Receiver<Result<ChatStreamDelta, AiError>>,
    ) {
        let (events, receiver) = tokio::sync::mpsc::channel(MCP_EVENT_CAPACITY);
        let executor: ToolExecutor = Arc::new(|call: ToolCall| {
            Box::pin(async move { Ok(format!("scoped result from {}", call.name)) })
        });
        (
            McpBridgeState {
                token: "one-turn-token".to_string(),
                tools: Arc::new(vec![ChatTool {
                    name: "temps".to_string(),
                    description: "Existing Temps virtual CLI".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }]),
                executor,
                events,
                tool_slot: Arc::new(Semaphore::new(1)),
                tool_timeout: MCP_TOOL_TIMEOUT,
            },
            receiver,
        )
    }

    fn authorized_headers() -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer one-turn-token".parse().expect("valid header"),
        );
        headers
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("MCP response body");
        serde_json::from_slice(&body).expect("MCP JSON response")
    }

    #[tokio::test]
    async fn mcp_bridge_rejects_missing_turn_token() {
        let (state, _events) = test_mcp_state();
        let response = mcp_bridge_handler(
            axum::extract::State(state),
            axum::http::HeaderMap::new(),
            axum::Json(serde_json::json!({"id": 1, "method": "initialize"})),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mcp_bridge_initializes_lists_and_executes_scoped_tool() {
        let (state, mut events) = test_mcp_state();
        let initialized = response_json(
            mcp_bridge_handler(
                axum::extract::State(state.clone()),
                authorized_headers(),
                axum::Json(serde_json::json!({"id": 1, "method": "initialize"})),
            )
            .await,
        )
        .await;
        assert_eq!(initialized["result"]["serverInfo"]["name"], "temps-chat");

        let listed = response_json(
            mcp_bridge_handler(
                axum::extract::State(state.clone()),
                authorized_headers(),
                axum::Json(serde_json::json!({"id": 2, "method": "tools/list"})),
            )
            .await,
        )
        .await;
        assert_eq!(listed["result"]["tools"][0]["name"], "temps");

        let called = response_json(
            mcp_bridge_handler(
                axum::extract::State(state),
                authorized_headers(),
                axum::Json(serde_json::json!({
                    "id": 3,
                    "method": "tools/call",
                    "params": {"name": "temps", "arguments": {"command": "projects list"}}
                })),
            )
            .await,
        )
        .await;
        assert_eq!(
            called["result"]["content"][0]["text"],
            "scoped result from temps"
        );
        assert!(matches!(
            events.recv().await,
            Some(Ok(ChatStreamDelta::ToolCall(_)))
        ));
        assert!(matches!(
            events.recv().await,
            Some(Ok(ChatStreamDelta::ToolResult { .. }))
        ));
    }

    #[tokio::test]
    async fn mcp_bridge_initialized_notification_has_no_response_body() {
        let (state, _events) = test_mcp_state();
        let response = mcp_bridge_handler(
            axum::extract::State(state),
            authorized_headers(),
            axum::Json(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("notification response body");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn mcp_bridge_rejects_a_second_concurrent_tool_call() {
        let (state, _events) = test_mcp_state();
        let _active = state
            .tool_slot
            .clone()
            .acquire_owned()
            .await
            .expect("tool semaphore open");
        let response = mcp_bridge_handler(
            axum::extract::State(state),
            authorized_headers(),
            axum::Json(serde_json::json!({
                "id": 4,
                "method": "tools/call",
                "params": {"name": "temps", "arguments": {}}
            })),
        )
        .await;
        let body = response_json(response).await;

        assert_eq!(body["result"]["isError"], true);
        assert!(body["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("already running")));
    }

    #[tokio::test]
    async fn mcp_bridge_bounds_tool_execution_time() {
        let (mut state, mut events) = test_mcp_state();
        state.tool_timeout = Duration::from_millis(10);
        state.executor = Arc::new(|_call| Box::pin(std::future::pending()));
        let response = mcp_bridge_handler(
            axum::extract::State(state),
            authorized_headers(),
            axum::Json(serde_json::json!({
                "id": 5,
                "method": "tools/call",
                "params": {"name": "temps", "arguments": {}}
            })),
        )
        .await;
        let body = response_json(response).await;

        assert_eq!(body["result"]["isError"], true);
        assert!(body["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("timed out")));
        assert!(matches!(
            events.recv().await,
            Some(Ok(ChatStreamDelta::ToolCall(_)))
        ));
        assert!(matches!(
            events.recv().await,
            Some(Ok(ChatStreamDelta::ToolResult { result, .. })) if result.contains("timed out")
        ));
    }

    // These two tests exercise the built router (rather than calling
    // `mcp_bridge_handler` directly, as the tests above do) because
    // `DefaultBodyLimit` is enforced by the extractor via a `tower::Layer`,
    // which only runs when a request actually passes through the router.
    #[tokio::test]
    async fn mcp_bridge_router_rejects_request_over_body_limit() {
        use tower::ServiceExt;
        let (state, _events) = test_mcp_state();
        let router = build_bridge_router("/mcp/test", state);
        let oversized_body = vec![b'a'; MCP_BODY_LIMIT_BYTES + 1];
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp/test")
            .header(axum::http::header::AUTHORIZATION, "Bearer one-turn-token")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(oversized_body))
            .expect("valid request");

        let response = router.oneshot(request).await.expect("router responds");

        assert_eq!(response.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn mcp_bridge_router_accepts_realistic_sized_tool_call() {
        use tower::ServiceExt;
        let (state, _events) = test_mcp_state();
        let router = build_bridge_router("/mcp/test", state);
        // Real MCP tool-call payloads (JSON-RPC envelope plus tool name and
        // arguments) run from a few hundred bytes to a handful of KB --
        // nowhere near MCP_BODY_LIMIT_BYTES. A large arguments blob (e.g. a
        // pasted file) can reach the low hundreds of KB, still well under
        // the 1 MiB limit; this asserts the limit doesn't clip that range.
        let body = serde_json::json!({
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "temps",
                "arguments": {"command": "projects list", "padding": "x".repeat(200 * 1024)}
            }
        })
        .to_string();
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp/test")
            .header(axum::http::header::AUTHORIZATION, "Bearer one-turn-token")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body))
            .expect("valid request");

        let response = router.oneshot(request).await.expect("router responds");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn mcp_bridge_emits_terminal_result_when_executor_fails() {
        let (mut state, mut events) = test_mcp_state();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_executor = calls.clone();
        state.executor = Arc::new(move |_call| {
            calls_for_executor.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(AiError::Provider {
                    purpose: "test.tool".to_string(),
                    reason: "executor failed".to_string(),
                })
            })
        });
        let response = mcp_bridge_handler(
            axum::extract::State(state),
            authorized_headers(),
            axum::Json(serde_json::json!({
                "id": 6,
                "method": "tools/call",
                "params": {"name": "temps", "arguments": {}}
            })),
        )
        .await;
        let body = response_json(response).await;

        assert_eq!(body["result"]["isError"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            events.recv().await,
            Some(Ok(ChatStreamDelta::ToolCall(_)))
        ));
        assert!(matches!(
            events.recv().await,
            Some(Ok(ChatStreamDelta::ToolResult { result, .. })) if result.contains("executor failed")
        ));
    }

    #[tokio::test]
    async fn mcp_bridge_shutdown_aborts_a_stuck_server_task() {
        let (shutdown, _shutdown_rx) = tokio::sync::oneshot::channel();
        let (_events_tx, events) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(std::future::pending::<()>());
        let mut bridge = ScopedMcpBridge {
            config: temps_agents::ai_cli::McpServerConfig {
                url: "http://127.0.0.1:1/mcp/test".to_string(),
                authorization_token: "test".to_string(),
            },
            events,
            shutdown: Some(shutdown),
            task,
        };

        bridge
            .shutdown_with_timeout(Duration::from_millis(10))
            .await;
        assert!(bridge.task.is_finished());
    }

    // -----------------------------------------------------------------------
    // Test 6: chat_stream_turn() always returns NotAvailable, even tool-less
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_chat_stream_turn_always_returns_not_available() {
        let called = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: "irrelevant".into(),
            model: None,
            called: called.clone(),
        });

        let scratch = tempfile::tempdir().unwrap();
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            2,
        );

        // Deliberately tool-less: this is the exact request shape that
        // chat_stream() would otherwise happily execute via the CLI.
        let request = ChatTurnRequest {
            purpose: "test.chat_turn".into(),
            tools: vec![],
            ..Default::default()
        };

        let result = service.chat_stream_turn(request).await;

        assert!(
            matches!(result, Err(AiError::NotAvailable)),
            "expected NotAvailable even for a tool-less request, got: {:?}",
            result.err()
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "provider.run() must not be called via chat_stream_turn()"
        );
    }

    #[tokio::test]
    async fn managed_workspace_turn_never_falls_back_to_the_host_cli() {
        let called = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: "host output must never be used".into(),
            model: None,
            called: called.clone(),
        });
        let scratch = tempfile::tempdir().unwrap();
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            1,
        );
        let request = ChatTurnRequest {
            purpose: "chat.application".into(),
            messages: vec![temps_ai::ChatMessage::user("make a file")],
            harness_workspace: Some(temps_ai::HarnessWorkspace {
                sandbox_label: "app_safe".into(),
                host_work_dir: scratch.path().join("ai-applications").join("app_safe"),
            }),
            ..Default::default()
        };

        let result = service
            .chat_stream_turn_with_services(request, TurnServices::default())
            .await;

        assert!(matches!(
            result,
            Err(AiError::Provider { reason, .. }) if reason.contains("Temps sandbox execution is not configured")
        ));
        assert!(
            !called.load(Ordering::SeqCst),
            "a managed workspace request must fail closed instead of using the host CLI"
        );
    }

    #[test]
    fn sandbox_harness_command_respects_claude_runtime_controls() {
        let request = ChatTurnRequest {
            purpose: "chat.application".into(),
            model: Some("claude-opus-5".into()),
            thinking_level: Some("high".into()),
            permission_mode: Some("plan".into()),
            ..Default::default()
        };

        let command = sandbox_harness_command("claude_cli", "build it", &request)
            .expect("Claude sandbox command should build");

        assert!(command
            .windows(2)
            .any(|pair| pair == ["--model", "claude-opus-5"]));
        assert!(command.windows(2).any(|pair| pair == ["--effort", "high"]));
        assert!(command
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "plan"]));
        assert!(
            !command
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions"),
            "a selected permission mode must not be silently bypassed"
        );
    }

    #[test]
    fn sandbox_harness_commands_resume_each_native_provider_session() {
        let request = ChatTurnRequest {
            purpose: "chat.application".into(),
            resume_session_id: Some("session-123".into()),
            ..Default::default()
        };

        let claude = sandbox_harness_command("claude_cli", "continue", &request)
            .expect("Claude sandbox command should build");
        assert!(claude
            .windows(2)
            .any(|pair| pair == ["--resume", "session-123"]));

        let codex = sandbox_harness_command("codex_cli", "continue", &request)
            .expect("Codex sandbox command should build");
        assert_eq!(
            &codex[..5],
            ["codex", "exec", "resume", "session-123", "continue"]
        );

        let opencode = sandbox_harness_command("opencode", "continue", &request)
            .expect("OpenCode sandbox command should build");
        assert!(opencode
            .windows(2)
            .any(|pair| pair == ["--session", "session-123"]));
    }

    #[test]
    fn resumed_sandbox_prompt_sends_only_the_latest_user_turn() {
        let request = ChatTurnRequest {
            purpose: "chat.application".into(),
            resume_session_id: Some("session-123".into()),
            messages: vec![
                temps_ai::ChatMessage::system("large live platform context"),
                temps_ai::ChatMessage::user("old request"),
                temps_ai::ChatMessage::assistant("old response"),
                temps_ai::ChatMessage::user("new request"),
            ],
            ..Default::default()
        };

        assert_eq!(build_sandbox_chat_prompt(&request), "new request");
    }

    #[test]
    fn fresh_sandbox_prompt_replays_server_owned_history() {
        let request = ChatTurnRequest {
            purpose: "chat.application".into(),
            messages: vec![
                temps_ai::ChatMessage::system("platform context"),
                temps_ai::ChatMessage::user("first request"),
            ],
            ..Default::default()
        };

        assert_eq!(
            build_sandbox_chat_prompt(&request),
            "[system]\nplatform context\n\n[user]\nfirst request"
        );
    }

    #[test]
    fn sandbox_harness_command_places_opencode_flags_before_prompt() {
        let request = ChatTurnRequest {
            purpose: "chat.application".into(),
            model: Some("openai/gpt-5.6".into()),
            thinking_level: Some("max".into()),
            permission_mode: Some("build".into()),
            ..Default::default()
        };

        let command = sandbox_harness_command("opencode", "build it", &request)
            .expect("OpenCode sandbox command should build");
        let prompt = command
            .iter()
            .position(|arg| arg == "build it")
            .expect("prompt is present");
        let agent = command
            .iter()
            .position(|arg| arg == "--agent")
            .expect("agent flag is present");
        let variant = command
            .iter()
            .position(|arg| arg == "--variant")
            .expect("variant flag is present");
        assert!(agent < prompt && variant < prompt);
    }

    #[test]
    fn sandbox_claude_registers_scoped_mcp_without_putting_bearer_in_arguments() {
        let request = ChatTurnRequest {
            purpose: "chat.application".into(),
            ..Default::default()
        };
        let mut command = sandbox_harness_command("claude_cli", "build it", &request)
            .expect("Claude sandbox command should build");
        let mut environment = HashMap::new();
        let mut secret_files = Vec::new();
        let server = temps_ai::HarnessMcpServer {
            url: "http://host.docker.internal:8080/api/ai/sandbox-tools/id/mcp".to_string(),
            authorization_token: "tmcp_super_secret".to_string(),
        };

        let cleanup = configure_sandbox_mcp(
            "claude_cli",
            &mut command,
            &mut environment,
            &mut secret_files,
            Some(&server),
        )
        .expect("MCP configuration should be generated");

        let cleanup = cleanup.expect("Claude uses a temporary MCP config");
        assert!(cleanup.starts_with("/run/secrets/temps-chat-mcp-"));
        assert!(command
            .windows(2)
            .any(|pair| pair == ["--mcp-config", cleanup.as_str()]));
        assert!(command
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "mcp__temps-chat"]));
        assert!(command.windows(2).any(|pair| {
            pair == [
                "--permission-prompt-tool",
                "mcp__temps-chat__temps_native_permission",
            ]
        }));
        assert!(!command
            .iter()
            .any(|argument| argument.contains("super_secret")));
        let config: serde_json::Value =
            serde_json::from_slice(&secret_files[0].1).expect("valid MCP JSON");
        assert_eq!(
            config["mcpServers"]["temps-chat"]["headers"]["Authorization"],
            "Bearer tmcp_super_secret"
        );
        assert_eq!(
            sandbox_mcp_cleanup_command(&cleanup),
            ["rm", "-f", "--", cleanup.as_str()]
        );
    }

    #[test]
    fn sandbox_codex_registers_scoped_mcp_through_process_environment() {
        let request = ChatTurnRequest {
            purpose: "chat.application".into(),
            ..Default::default()
        };
        let mut command = sandbox_harness_command("codex_cli", "build it", &request)
            .expect("Codex sandbox command should build");
        let mut environment = HashMap::new();
        let mut secret_files = Vec::new();
        let server = temps_ai::HarnessMcpServer {
            url: "http://host.docker.internal:8080/api/ai/sandbox-tools/id/mcp".to_string(),
            authorization_token: "tmcp_super_secret".to_string(),
        };

        configure_sandbox_mcp(
            "codex_cli",
            &mut command,
            &mut environment,
            &mut secret_files,
            Some(&server),
        )
        .expect("MCP configuration should be generated");

        assert!(command.iter().any(|argument| {
            argument == "mcp_servers.temps_chat.bearer_token_env_var=\"TEMPS_CHAT_MCP_TOKEN\""
        }));
        assert!(!command
            .iter()
            .any(|argument| argument.contains("super_secret")));
        assert_eq!(
            environment.get("TEMPS_CHAT_MCP_TOKEN").map(String::as_str),
            Some("tmcp_super_secret")
        );
        assert!(secret_files.is_empty());
    }

    #[test]
    fn sandbox_claude_receives_only_the_model_relay_capability() {
        let relay = SandboxModelRelay {
            base_url: "https://temps.example.test/api/ai/sandbox-models/id".to_string(),
            bearer: "tmodel_short_lived".to_string(),
        };
        let mut environment = HashMap::new();

        configure_sandbox_model_relay("claude_cli", &mut environment, &relay)
            .expect("Claude relay should be configured");

        assert_eq!(
            environment.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://temps.example.test/api/ai/sandbox-models/id")
        );
        assert_eq!(
            environment.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str),
            Some("tmodel_short_lived")
        );
        assert!(!environment.contains_key("ANTHROPIC_API_KEY"));
        assert!(!environment.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    // -----------------------------------------------------------------------
    // Test 7: oversized prompt is rejected before any resource is acquired
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_complete_rejects_oversized_prompt_without_acquiring_resources() {
        let called = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: "irrelevant".into(),
            model: None,
            called: called.clone(),
        });

        let scratch = tempfile::tempdir().unwrap();
        // concurrency_limit 1 makes it easy to prove no permit was held: if
        // check_prompt_size() ran after acquiring, a second call would fail
        // with "concurrency limit reached" instead of the size error.
        let service = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            1,
        );

        let oversized_prompt = "x".repeat(MAX_PROMPT_BYTES + 1);
        let result = service
            .complete(AiRequest {
                purpose: "test.oversized".into(),
                prompt: oversized_prompt,
                ..Default::default()
            })
            .await;

        match &result {
            Err(AiError::Provider { purpose, reason }) => {
                assert_eq!(purpose, "test.oversized");
                assert!(
                    reason.contains("exceeds maximum size"),
                    "expected a size-limit reason, got: {}",
                    reason
                );
            }
            other => panic!("expected AiError::Provider, got: {:?}", other),
        }
        assert!(
            !called.load(Ordering::SeqCst),
            "provider.run() must not be called for an oversized prompt"
        );

        // The rejected call must not have held the sole permit: a normal
        // request should still succeed right after.
        let ok = service
            .complete(AiRequest {
                purpose: "test.after_oversized".into(),
                prompt: "small".into(),
                ..Default::default()
            })
            .await;
        assert!(
            ok.is_ok(),
            "a normal request after an oversized one should still succeed, got: {:?}",
            ok
        );
    }

    // -----------------------------------------------------------------------
    // Test 8: constructing with concurrency_limit = 0 panics
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[should_panic(expected = "concurrency_limit must be at least 1")]
    async fn test_new_panics_on_zero_concurrency_limit() {
        let provider: Arc<dyn AiCliProvider> = Arc::new(MockProvider {
            status: available_status(),
            output: String::new(),
            model: None,
            called: Arc::new(AtomicBool::new(false)),
        });
        let scratch = tempfile::tempdir().unwrap();
        let _ = AgentCliAiService::new(
            provider,
            scratch.path().to_owned(),
            Duration::from_secs(30),
            0,
        );
    }

    #[test]
    fn claude_session_title_uses_current_user_segment_from_flattened_prompt() {
        let transcript = br#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[system]\nOperate Temps safely.\n\n[user]\nCan you create a MongoDB instance?"}]}}
"#;
        assert_eq!(
            claude_session_title_from_transcript(transcript).as_deref(),
            Some("Can you create a MongoDB instance?")
        );
    }

    #[test]
    fn claude_session_title_prefers_provider_custom_title() {
        let transcript = br#"{"type":"custom-title","customTitle":"  MongoDB   Service Setup  "}
{"type":"user","message":{"role":"user","content":"ignored"}}
"#;
        assert_eq!(
            claude_session_title_from_transcript(transcript).as_deref(),
            Some("MongoDB Service Setup")
        );
    }
}
