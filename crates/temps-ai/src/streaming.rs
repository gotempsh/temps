//! Multi-turn, streaming chat types for the AI foundation (ADR-023).
//!
//! Where [`crate::AiService::complete`] is a single request→response,
//! [`crate::AiService::chat_stream`] takes a replayed message history and streams
//! the assistant's reply token-by-token — the substrate for persistent,
//! resumable debugging conversations.

use std::pin::Pin;

use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::service::AiError;

/// Who authored a turn. Mirrors the OpenAI-compatible roles the gateway accepts.
pub const ROLE_SYSTEM: &str = "system";
pub const ROLE_USER: &str = "user";
pub const ROLE_ASSISTANT: &str = "assistant";
pub const ROLE_TOOL: &str = "tool";

/// A tool the model may call during a turn (OpenAI "function" tool schema).
/// Providers expose these per conversation context (e.g. read a repo file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTool {
    /// Function name the model emits in a tool call, e.g. `"read_repo_file"`.
    pub name: String,
    /// Natural-language description so the model knows when/how to call it.
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// A tool invocation the model requested. `arguments` is the raw JSON string the
/// model emitted (parse it in the executor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// One turn of a conversation. Deliberately flat so it is provider-agnostic and
/// trivially persisted as an `ai_messages` row. `tool_calls` / `tool_call_id`
/// are only populated transiently during an agentic tool loop and are never
/// stored — persisted turns only ever carry `role` + `content`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"system"` | `"user"` | `"assistant"` | `"tool"`.
    pub role: String,
    pub content: String,
    /// Tool calls the assistant requested this turn (assistant messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Which tool call this message answers (`tool` role messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ROLE_SYSTEM.into(),
            content: content.into(),
            ..Default::default()
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ROLE_USER.into(),
            content: content.into(),
            ..Default::default()
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ROLE_ASSISTANT.into(),
            content: content.into(),
            ..Default::default()
        }
    }
    /// A tool-result message answering the tool call `tool_call_id`.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ROLE_TOOL.into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A multi-turn request. The caller supplies the *full* replayed history (our DB
/// is the source of truth — see ADR-023); the provider is stateless. When
/// `tools` is non-empty the model may answer with tool calls instead of text
/// (see [`crate::AiService::chat`]).
#[derive(Debug, Clone, Default)]
pub struct ChatTurnRequest {
    /// Short tag for logging / usage attribution, e.g. `"deploy.debug_chat"`.
    pub purpose: String,
    /// Governance + usage scope.
    pub project_id: Option<i32>,
    /// Full conversation history, oldest first (system prompt usually first).
    pub messages: Vec<ChatMessage>,
    /// Tools the model may call this turn. Empty = plain chat.
    pub tools: Vec<ChatTool>,
    /// Override the configured default model.
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

/// A single non-streaming turn result: either assistant text, or a set of tool
/// calls to execute and feed back (an agentic step). Both may be present (some
/// models narrate before calling a tool).
#[derive(Debug, Clone, Default)]
pub struct ChatTurnResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

/// A stream of assistant text deltas. Each `Ok(String)` is an incremental chunk
/// to append; the stream ends when the reply is complete. Errors are terminal.
pub type TokenStream = Pin<Box<dyn Stream<Item = Result<String, AiError>> + Send>>;

/// One delta from a streaming *agentic* turn ([`crate::AiService::chat_stream_turn`]).
/// A single provider pass can interleave assistant text and tool calls: the
/// OpenAI/Anthropic streaming APIs emit tool-call argument fragments inline, so
/// the provider accumulates them and surfaces each as a fully-assembled
/// [`ToolCall`] once complete. This is what lets the UI show tool activity *and*
/// streamed prose from a single model call (the same thing the Vercel AI SDK
/// does), instead of a separate non-streaming gather pass.
#[derive(Debug, Clone)]
pub enum ChatStreamDelta {
    /// An incremental chunk of assistant prose to append to the message.
    Text(String),
    /// A fully-assembled tool call the model decided to make this turn.
    ToolCall(ToolCall),
}

/// A stream of [`ChatStreamDelta`]s for one agentic turn. Errors are terminal.
pub type ChatTurnStream = Pin<Box<dyn Stream<Item = Result<ChatStreamDelta, AiError>> + Send>>;
