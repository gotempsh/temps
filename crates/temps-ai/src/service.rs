//! The object-safe [`AiService`] seam and its request/response types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::streaming::{
    ChatStreamDelta, ChatTurnRequest, ChatTurnResponse, ChatTurnStream, TokenStream,
};

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

    /// Low-level completion. Prefer the [`crate::complete_text`] /
    /// [`crate::complete_typed`] helpers for everyday use.
    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError>;

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
}
