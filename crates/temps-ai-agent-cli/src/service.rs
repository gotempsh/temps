// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`AgentCliAiService`]: an [`AiService`] implementation that delegates
//! eligible workloads to a subscription-backed agent CLI.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::response::{IntoResponse, Response};
use tokio::sync::Semaphore;

use temps_agents::ai_cli::{
    scrub_and_bound, AiCliProvider, AiRunConfig, OnEventCallback, PermissionBridge,
};
use temps_agents::error::AgentError;
use temps_ai::{
    extract_json_block, AiError, AiRequest, AiResponse, AiService, ChatStreamDelta, ChatTool,
    ChatTurnRequest, ChatTurnStream, TokenStream, ToolCall, ToolExecutor, TurnServices,
};

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
/// `api_key` in every [`AiRunConfig`] is deliberately `""`. The operator's
/// credential is already seeded into the CLI's standard config path on the host
/// (`~/.claude/.credentials.json`, etc.) by the existing settings flow. This
/// service never reads or forwards the credential (ADR-037 §4).
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
        }
    }
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
        let status = self.provider.get_status().await;
        status.installed && status.authenticated
    }

    async fn capabilities_for(
        &self,
        _provider: Option<&str>,
        _refresh: temps_ai::RefreshPolicy,
    ) -> Result<temps_ai::ProviderCapabilities, AiError> {
        let status = self.provider.get_status().await;
        if !status.installed || !status.authenticated {
            return Err(AiError::NotAvailable);
        }
        self.provider
            .capabilities()
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: "provider.capabilities".to_string(),
                reason: format!(
                    "provider '{}' has no registered capability contract",
                    self.provider.name()
                ),
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
        let on_event: OnEventCallback = Arc::new(move |line: String| {
            let provider_name = provider_name.clone();
            let callback_tx = callback_tx.clone();
            let streamed_partial = streamed_partial.clone();
            Box::pin(async move {
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

    use temps_agents::ai_cli::{AiCliStatus, AiRunResult};
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
}
