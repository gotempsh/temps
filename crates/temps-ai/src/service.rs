// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The object-safe [`AiService`] seam and its request/response types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::streaming::{
    ChatMessage, ChatStreamDelta, ChatTurnRequest, ChatTurnResponse, ChatTurnStream, TokenStream,
    ToolExecutor, TurnServices,
};
use crate::{ProviderCapabilities, RefreshPolicy};

/// A single AI completion request. Construct with `..Default::default()` and set
/// only what you need:
///
/// ```ignore
/// let req = AiRequest { purpose: "alert.summary".into(), prompt, ..Default::default() };
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiRequest {
    /// Short tag for logging, usage attribution, and per-purpose budgets,
    /// e.g. `"alert.summary"` or `"deploy.build_diagnosis"`.
    pub purpose: String,
    /// Optional governance + usage scope (per-project budgets / allow-lists).
    pub project_id: Option<i32>,
    /// Optional explicit provider selection; `None` uses the instance default.
    pub provider: Option<String>,
    /// Optional system instruction.
    pub system: Option<String>,
    /// The user prompt.
    pub prompt: String,
    /// Override the configured default model for this call.
    pub model: Option<String>,
    /// Cap on response tokens (provider default when `None`).
    pub max_tokens: Option<u32>,
    /// Sampling temperature (provider default when `None`).
    pub temperature: Option<f32>,
    /// Provider-normalized reasoning depth. Callers must validate this value
    /// against [`ProviderCapabilities`] before dispatching it.
    pub thinking_level: Option<String>,
    /// When set, the provider is asked to return JSON matching this JSON Schema.
    /// Usually populated by [`crate::complete_typed`] from a Rust type rather than
    /// by hand.
    pub response_schema: Option<serde_json::Value>,
}

/// The result of a completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiResponse {
    /// The assistant's text reply.
    pub text: String,
    /// Parsed JSON, when a schema was requested and the reply parsed as JSON.
    pub json: Option<serde_json::Value>,
    /// The model that actually served the request.
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct NativeSessionExportRequest {
    pub principal_id: i32,
    pub provider: String,
    pub session_id: String,
    pub harness_workspace: crate::HarnessWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeSessionExport {
    pub provider: String,
    pub session_id: String,
    pub format: String,
    pub json: String,
    pub truncated: bool,
}

/// One server-authorized operation against the managed process supervisor in
/// an already-running application sandbox.
#[derive(Debug, Clone)]
pub struct RuntimeProcessRequest {
    pub principal_id: i32,
    pub provider: String,
    pub harness_workspace: crate::HarnessWorkspace,
    pub operation: RuntimeProcessOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeProcessOperation {
    Start {
        idempotency_key: String,
        name: String,
        program: String,
        args: Vec<String>,
        directory: String,
        restart: bool,
    },
    Status {
        process_id: String,
    },
    Logs {
        process_id: String,
        after_sequence: Option<u64>,
        limit: Option<u16>,
    },
    Stop {
        process_id: String,
    },
    Restart {
        process_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeProcessSnapshot {
    pub id: String,
    pub name: String,
    pub status: String,
    pub detail: String,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeProcessLogLine {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub stream: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeProcessResponse {
    Process {
        process: RuntimeProcessSnapshot,
    },
    Logs {
        process_id: String,
        lines: Vec<RuntimeProcessLogLine>,
        next_sequence: Option<u64>,
        truncated: bool,
    },
}

/// Why an AI call could not be completed. All variants are non-fatal — callers
/// fall back to non-AI behaviour.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// No provider key / usable model is configured. Check [`AiService::is_available`]
    /// first to avoid building a prompt that can't be served.
    #[error("AI is not configured (no provider key or usable model)")]
    NotAvailable,
    /// No model could be resolved for this request.
    #[error("no model configured for AI request '{purpose}'")]
    NoModel { purpose: String },
    /// The provider/gateway returned an error.
    #[error("AI provider error for '{purpose}': {reason}")]
    Provider { purpose: String, reason: String },
    /// Retained runtime diagnostic after exact turn-secret redaction at source.
    #[error("retained AI harness error for '{purpose}': {reason}")]
    RetainedHarnessDiagnostic { purpose: String, reason: String },
}

/// The governed AI capability. Object-safe so it can be registered and resolved
/// as `Arc<dyn AiService>` through the plugin DI.
///
/// Implementations route through the AI gateway, inheriting provider-key
/// resolution, model routing, and per-scope rate/cost governance. They are
/// best-effort: never panic, and never block beyond the work itself (the caller
/// adds the timeout).
#[async_trait]
pub trait AiService: Send + Sync {
    /// Cheap gate: is a provider key + usable model actually configured? Lets a
    /// caller skip prompt construction when AI is unavailable.
    async fn is_available(&self) -> bool;

    /// Provider-aware availability for a resource pinned to an immutable route.
    async fn is_available_for(&self, _provider: Option<&str>) -> bool {
        self.is_available().await
    }

    /// Cheap gate for the multi-turn tool-calling workload specifically
    /// (debugging chat, propose-then-confirm write actions — anything that
    /// calls [`Self::chat`]/[`Self::chat_stream_turn`]). Defaults to
    /// [`Self::is_available`] for implementations where the two coincide.
    ///
    /// This is a distinct method — not just a reuse of `is_available` — for
    /// implementations that serve *some* workloads but not tool-calling
    /// (e.g. a subscription agent CLI: it can do plain completions, but has
    /// no external function-calling protocol to hand it). Those
    /// implementations override this to report `false` even while
    /// `is_available` reports `true`, so a readiness check gated on the
    /// right method never tells a caller "configured" for a workload the
    /// active provider can't actually serve.
    async fn chat_capable(&self) -> bool {
        self.is_available().await
    }

    /// Provider-aware readiness for a pinned conversation.
    async fn chat_capable_for(&self, _provider: Option<&str>) -> bool {
        self.chat_capable().await
    }

    /// Return the normalized controls and realtime features for one provider.
    /// UI and conversation validation consume this contract, so adding an
    /// adapter never requires provider-id branches in either layer.
    async fn capabilities_for(
        &self,
        _provider: Option<&str>,
        _refresh: RefreshPolicy,
    ) -> Result<ProviderCapabilities, AiError> {
        Err(AiError::NotAvailable)
    }

    /// Return capabilities for the provider credential available to the Temps
    /// host. The snapshot preserves whether discovery was live, cached, stale,
    /// or only a bootstrap fallback.
    async fn capabilities_snapshot_for(
        &self,
        provider: Option<&str>,
        refresh: RefreshPolicy,
    ) -> Result<crate::ProviderCapabilitiesSnapshot, AiError> {
        let capabilities = self.capabilities_for(provider, refresh).await?;
        Ok(crate::ProviderCapabilitiesSnapshot {
            capabilities,
            model_source: match refresh {
                RefreshPolicy::Refresh => crate::ModelCatalogSource::Live,
                RefreshPolicy::Cached => crate::ModelCatalogSource::Cache,
            },
            models_refreshed_at: None,
        })
    }

    /// Return provider capabilities for the credential used by a user's
    /// persistent workspace. Implementations without a distinct workspace
    /// credential inherit the ordinary provider capability path.
    async fn capabilities_snapshot_for_principal(
        &self,
        provider: Option<&str>,
        _principal_id: i32,
        refresh: RefreshPolicy,
    ) -> Result<crate::ProviderCapabilitiesSnapshot, AiError> {
        self.capabilities_snapshot_for(provider, refresh).await
    }

    /// Invalidate account-scoped capability state after credentials change.
    async fn invalidate_capabilities_for(&self, _provider: Option<&str>) {}

    /// Export a provider-native session after the caller has resolved and
    /// authorized its persistent workspace. Implementations must sanitize and
    /// bound the payload; `None` means the selected provider has no native
    /// export contract.
    async fn export_native_session(
        &self,
        _request: NativeSessionExportRequest,
    ) -> Result<Option<NativeSessionExport>, AiError> {
        Ok(None)
    }

    /// Operate the runtime daemon's bounded process supervisor in a workspace
    /// that the caller has already authorized. Implementations must not create,
    /// wake, or replace a sandbox while serving this method.
    async fn runtime_process(
        &self,
        _request: RuntimeProcessRequest,
    ) -> Result<RuntimeProcessResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    /// Low-level completion. Prefer the [`crate::complete_text`] /
    /// [`crate::complete_typed`] helpers for everyday use.
    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError>;

    /// Stream a single-pass completion. Providers with native structured
    /// streaming override this method so `response_schema` is enforced by the
    /// provider. The default adapter keeps every [`AiService`] reusable: it
    /// translates the request into a tool-less chat stream and embeds the JSON
    /// Schema in the system instruction for providers (notably host CLIs) that
    /// cannot accept a native response-format argument.
    async fn complete_stream(&self, request: AiRequest) -> Result<TokenStream, AiError> {
        let mut system = request.system.unwrap_or_default();
        if let Some(schema) = request.response_schema {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(
                "Return only one JSON object matching this JSON Schema. Do not use Markdown fences:\n",
            );
            system.push_str(&schema.to_string());
        }

        let mut messages = Vec::with_capacity(2);
        if !system.is_empty() {
            messages.push(ChatMessage::system(system));
        }
        messages.push(ChatMessage::user(request.prompt));

        self.chat_stream(ChatTurnRequest {
            purpose: request.purpose,
            project_id: request.project_id,
            provider: request.provider,
            messages,
            model: request.model,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            thinking_level: request.thinking_level,
            ..Default::default()
        })
        .await
    }

    /// Multi-turn streaming completion (ADR-023): replays the supplied history
    /// and streams the assistant reply token-by-token. The substrate for
    /// persistent debugging conversations. Best-effort like [`Self::complete`].
    async fn chat_stream(&self, request: ChatTurnRequest) -> Result<TokenStream, AiError>;

    /// Non-streaming multi-turn completion that supports tool calling. Returns
    /// either assistant text or a set of tool calls to execute and feed back
    /// (one step of an agentic loop). Default impl reports no tool support, so
    /// callers fall back to [`Self::chat_stream`]; the gateway implementation
    /// overrides it. Best-effort like [`Self::complete`].
    async fn chat(&self, _request: ChatTurnRequest) -> Result<ChatTurnResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    /// Streaming agentic turn: streams assistant text deltas **and** tool calls
    /// from a single provider pass (see [`ChatStreamDelta`]). This is the seam an
    /// agentic chat loop should use — it gives token-by-token prose *and* live
    /// tool activity without a separate non-streaming gather call.
    ///
    /// The default implementation adapts the text-only [`Self::chat_stream`]: it
    /// streams prose but never surfaces tool calls (so a model would just answer
    /// in text). Providers that can stream tool-call deltas — like the gateway —
    /// override this to honour `request.tools`. Best-effort like [`Self::complete`].
    async fn chat_stream_turn(&self, request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        use futures::StreamExt;
        let stream = self.chat_stream(request).await?;
        Ok(Box::pin(stream.map(|item| item.map(ChatStreamDelta::Text))))
    }

    /// Provider-native tool harness entry point. Gateway implementations use
    /// their normal tool-call stream and leave execution to the caller; CLI
    /// implementations override this to expose `request.tools` through an
    /// ephemeral scoped bridge and invoke `executor` from that bridge.
    async fn chat_stream_turn_with_executor(
        &self,
        request: ChatTurnRequest,
        _executor: Option<ToolExecutor>,
    ) -> Result<ChatTurnStream, AiError> {
        self.chat_stream_turn(request).await
    }

    /// Provider-neutral turn entry point. Tools, user interactions, streaming,
    /// and cancellation use the same runtime contract for gateway and harness
    /// adapters. The executor-only method remains as a compatibility shim.
    async fn chat_stream_turn_with_services(
        &self,
        request: ChatTurnRequest,
        services: TurnServices,
    ) -> Result<ChatTurnStream, AiError> {
        self.chat_stream_turn_with_executor(request, services.tools)
            .await
    }
}
