//! Multi-turn, streaming chat types for the AI foundation (ADR-023).
//!
//! Where [`crate::AiService::complete`] is a single request→response,
//! [`crate::AiService::chat_stream`] takes a replayed message history and streams
//! the assistant's reply token-by-token — the substrate for persistent,
//! resumable debugging conversations.

use std::pin::Pin;

use futures::future::BoxFuture;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

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

/// Request-scoped executor for tools advertised to an AI harness. The caller
/// owns authorization and project scoping; providers receive no platform
/// credential and can only invoke this opaque callback for the current turn.
pub type ToolExecutor =
    Arc<dyn Fn(ToolCall) -> BoxFuture<'static, Result<String, AiError>> + Send + Sync>;

/// Request-scoped bridge for provider-initiated user interactions (questions,
/// plan approval, or other normalized permission prompts). The runtime owns
/// persistence and authorization; adapters only await the returned decision.
pub type InteractionExecutor = Arc<
    dyn Fn(PermissionRequest) -> BoxFuture<'static, Result<PermissionDecision, AiError>>
        + Send
        + Sync,
>;

/// Common per-turn services supplied to every provider adapter. Adding a new
/// provider never creates a new chat entry point: adapters receive the same
/// scoped tools and interaction channel through this value.
#[derive(Clone, Default)]
pub struct TurnServices {
    pub tools: Option<ToolExecutor>,
    pub interactions: Option<InteractionExecutor>,
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
    /// Provider pinned by the caller for this conversation. `gateway` selects
    /// BYOK routing; an agent CLI catalog id selects that host CLI.
    pub provider: Option<String>,
    /// Full conversation history, oldest first (system prompt usually first).
    pub messages: Vec<ChatMessage>,
    /// Tools the model may call this turn. Empty = plain chat.
    pub tools: Vec<ChatTool>,
    /// Override the configured default model.
    pub model: Option<String>,
    /// Provider-specific reasoning/thinking option selected when the
    /// conversation was created (for example `high`).
    pub thinking_level: Option<String>,
    /// Provider-specific execution permission mode selected when the
    /// conversation was created (for example `auto` or `full-access`).
    pub permission_mode: Option<String>,
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

/// Kind of permission the Claude CLI is requesting via `--permission-prompt-tool stdio`
/// (ADR-038 Phase 2). Used to drive the correct UI card (`ToolApproval` → allow/deny
/// buttons; `Question` → answer form; `PlanApproval` → approve/reject-with-feedback).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    /// A tool invocation (`can_use_tool` subtype — Bash, Edit, Write, etc.).
    ToolApproval,
    /// The model wants to ask the user a question (`AskUserQuestion` tool).
    Question,
    /// The model wants the user to approve a plan (`ExitPlanMode` tool).
    PlanApproval,
}

/// A permission request emitted by `run_interactive` when the Claude CLI blocks
/// on a `control_request` frame.  Passed to the UI via an SSE event so the user
/// can respond before the subprocess continues.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PermissionRequest {
    /// The CLI's own `request_id` (UUID); used as the key in the pending-permission
    /// registry and as `{permission_id}` in the resolve endpoint.
    pub id: String,
    /// What kind of interaction is required.
    pub kind: PermissionKind,
    /// The tool name from `request.tool_name` (e.g. `"Bash"`, `"AskUserQuestion"`).
    pub tool_name: String,
    /// Raw `request.input` from the CLI — passed through to the UI verbatim so
    /// each milestone's card can render the relevant fields without requiring the
    /// service layer to know about tool-specific schemas.
    pub input: serde_json::Value,
}

/// The user's decision for a pending permission request.  Serialized as a tagged
/// JSON object and sent in the resolve endpoint body.  `DenyTool`/`RejectPlan`
/// carry an optional human-readable reason that is forwarded to the CLI's
/// `control_response` (never stored).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionDecision {
    /// Allow the tool to run as requested (milestone 3).
    AllowTool,
    /// Deny the tool, with an optional explanation for the model (milestone 3).
    DenyTool { reason: Option<String> },
    /// Answer an `AskUserQuestion` prompt (milestone 4).  `answers` is a JSON
    /// object mapping the literal question text to the chosen option label or
    /// free-text answer, per the confirmed wire protocol.
    AnswerQuestion { answers: serde_json::Value },
    /// Approve an `ExitPlanMode` plan (milestone 5).
    ApprovePlan,
    /// Reject an `ExitPlanMode` plan with optional feedback (milestone 5).
    RejectPlan { feedback: Option<String> },
}

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
    /// Result returned by a provider-native tool harness (for example an MCP
    /// call made inside a CLI process). Gateway providers never emit this: the
    /// outer conversation loop executes their `ToolCall` values itself.
    ToolResult { call: ToolCall, result: String },
    /// The interactive CLI subprocess is waiting for the user to approve or deny
    /// a tool/question/plan (ADR-038 Phase 2, milestone 3+).  The SSE handler
    /// emits this as a `permission_requested` event; the user resolves it via
    /// `POST .../permissions/{id}/resolve`, which unblocks the subprocess.
    PermissionRequested(PermissionRequest),
}

/// A stream of [`ChatStreamDelta`]s for one agentic turn. Errors are terminal.
pub type ChatTurnStream = Pin<Box<dyn Stream<Item = Result<ChatStreamDelta, AiError>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    // --- PermissionKind serde ---

    #[test]
    fn test_permission_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&PermissionKind::ToolApproval).unwrap(),
            r#""tool_approval""#
        );
        assert_eq!(
            serde_json::to_string(&PermissionKind::Question).unwrap(),
            r#""question""#
        );
        assert_eq!(
            serde_json::to_string(&PermissionKind::PlanApproval).unwrap(),
            r#""plan_approval""#
        );
    }

    #[test]
    fn test_permission_kind_round_trips() {
        for kind in [
            PermissionKind::ToolApproval,
            PermissionKind::Question,
            PermissionKind::PlanApproval,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: PermissionKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
    }

    // --- PermissionRequest serde ---

    #[test]
    fn test_permission_request_round_trip() {
        let req = PermissionRequest {
            id: "req-123".to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls -la"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: PermissionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, req.id);
        assert_eq!(back.kind, req.kind);
        assert_eq!(back.tool_name, req.tool_name);
        assert_eq!(back.input, req.input);
    }

    // --- PermissionDecision serde ---

    #[test]
    fn test_decision_allow_tool_round_trip() {
        let d = PermissionDecision::AllowTool;
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#""type":"allow_tool""#), "got: {json}");
        let back: PermissionDecision = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, PermissionDecision::AllowTool));
    }

    #[test]
    fn test_decision_deny_tool_with_reason_round_trip() {
        let d = PermissionDecision::DenyTool {
            reason: Some("not permitted".to_string()),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#""type":"deny_tool""#), "got: {json}");
        let back: PermissionDecision = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            PermissionDecision::DenyTool { reason: Some(ref r) } if r == "not permitted"
        ));
    }

    #[test]
    fn test_decision_deny_tool_without_reason_round_trip() {
        let d = PermissionDecision::DenyTool { reason: None };
        let json = serde_json::to_string(&d).unwrap();
        let back: PermissionDecision = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            PermissionDecision::DenyTool { reason: None }
        ));
    }

    #[test]
    fn test_decision_answer_question_round_trip() {
        let answers = serde_json::json!({"Do you want to proceed?": "Yes"});
        let d = PermissionDecision::AnswerQuestion {
            answers: answers.clone(),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#""type":"answer_question""#), "got: {json}");
        let back: PermissionDecision = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            &back,
            PermissionDecision::AnswerQuestion { answers: a } if *a == answers
        ));
    }

    #[test]
    fn test_decision_approve_plan_round_trip() {
        let d = PermissionDecision::ApprovePlan;
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#""type":"approve_plan""#), "got: {json}");
        let back: PermissionDecision = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, PermissionDecision::ApprovePlan));
    }

    #[test]
    fn test_decision_reject_plan_round_trip() {
        let d = PermissionDecision::RejectPlan {
            feedback: Some("not ready yet".to_string()),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#""type":"reject_plan""#), "got: {json}");
        let back: PermissionDecision = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            PermissionDecision::RejectPlan { feedback: Some(ref f) } if f == "not ready yet"
        ));
    }
}
