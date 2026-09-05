// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The conversation service: create/find/history + streaming `send_message`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, oneshot, Semaphore};
use tokio::task::AbortHandle;

use chrono::Utc;
use futures::{future::BoxFuture, Stream};
use futures_util::StreamExt;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set,
};

use temps_ai::{
    streaming::{PermissionDecision, PermissionKind, PermissionRequest},
    AiRequest, AiService, ChatMessage, ChatStreamDelta, ChatTool, ChatTurnRequest,
    HarnessMcpServer, ToolCall, ToolExecutor,
};
use temps_core::{AuditContext, AuditLogger, RequestMetadata};

/// One entry in the pending-permission registry (ADR-038 Phase 2).
///
/// Stores the one-shot sender together with the conversation it belongs to and
/// the kind of permission the CLI is waiting for.  Both fields are checked by
/// `resolve_permission` before the entry is consumed:
///
/// * `conv_public_id` prevents IDOR — an attacker who learns a `permission_id`
///   from session A cannot resolve it through session B's URL.
/// * `kind` prevents kind mismatch — a client cannot send an `AnswerQuestion`
///   decision for a `tool_approval` permission (and vice-versa).
pub struct PendingPermissionEntry {
    /// One-shot sender: consuming it (via `remove` + `send`) is the atomic
    /// claim that prevents double-resolution (409 semantic).
    pub sender: oneshot::Sender<PermissionResolution>,
    /// `public_id` of the conversation that registered this permission.
    pub conv_public_id: String,
    /// What kind of interaction the CLI subprocess is waiting for.
    pub kind: PermissionKind,
    /// The tool name from the original `control_request` (e.g. `"AskUserQuestion"`).
    /// Kept alongside `input` so a page reload can reconstruct the same
    /// interactive card instead of leaving the user with only the inert
    /// "asked" text message and no way to answer (ADR-038 Phase 2).
    pub tool_name: String,
    /// A redacted display copy of the original `control_request` input. This is
    /// sufficient for reconstructing the card but cannot contain raw tokens,
    /// passwords, secret flags, or platform write payload values.
    pub input: serde_json::Value,
    /// Unique registration generation. A cancelled older request must not
    /// remove a newer request that reused the same provider-supplied id.
    pub generation: uuid::Uuid,
    /// Server-owned provenance. Authorization must never be inferred from a
    /// provider-controlled tool name, which may collide with `temps_write`.
    pub origin: PendingPermissionOrigin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingPermissionOrigin {
    Provider,
    PlatformWrite,
}

/// Server-only resolution envelope. Provider adapters receive only the
/// normalized decision; platform writes additionally use the fresh auth and
/// request metadata from the human who clicked Approve. This prevents a long
/// running turn from executing with a role snapshot captured when it started.
pub struct PermissionResolution {
    pub decision: PermissionDecision,
    pub auth: AuthContext,
    pub metadata: RequestMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoApprovedPermission {
    pub id: String,
    pub tool_name: String,
    pub delivered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivePermissionModeUpdate {
    pub applied_to_active_turn: bool,
    pub auto_approved: Vec<AutoApprovedPermission>,
}

/// Internal interaction channel used by platform tools. Unlike the public
/// provider bridge, it retains the resolving principal so writes can re-check
/// authorization at execution time and emit a complete audit record.
type PlatformInteractionExecutor = Arc<
    dyn Fn(
            temps_ai::streaming::PermissionRequest,
        ) -> BoxFuture<'static, Result<PermissionResolution, temps_ai::AiError>>
        + Send
        + Sync,
>;

/// Removes one exact pending interaction when its waiter is completed or
/// cancelled. This binds approval-card lifetime to the provider turn instead
/// of leaving stale, actionable registry entries after Stop/timeout.
struct PendingPermissionGuard {
    registry: Arc<Mutex<HashMap<String, PendingPermissionEntry>>>,
    permission_id: String,
    generation: uuid::Uuid,
}

impl Drop for PendingPermissionGuard {
    fn drop(&mut self) {
        let mut registry = match self.registry.lock() {
            Ok(registry) => registry,
            Err(poisoned) => poisoned.into_inner(),
        };
        if registry
            .get(&self.permission_id)
            .is_some_and(|entry| entry.generation == self.generation)
        {
            registry.remove(&self.permission_id);
        }
    }
}

/// Cancels durable inline action rows when the active waiter is dropped by a
/// stop, timeout, panic, or process restart. The guard is disarmed only after a
/// terminal database transition has succeeded.
struct InlineActionGuard {
    pending: PendingActionService,
    action_public_ids: Vec<String>,
    armed: bool,
}

impl InlineActionGuard {
    fn new(pending: &PendingActionService, action_public_ids: Vec<String>) -> Self {
        Self {
            pending: pending.clone(),
            action_public_ids,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InlineActionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let pending = self.pending.clone();
        let action_public_ids = self.action_public_ids.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                pending
                    .cancel_inline(
                        &action_public_ids,
                        "The inline approval ended before a terminal decision was recorded.",
                    )
                    .await;
            });
        } else {
            tracing::error!(
                action_count = action_public_ids.len(),
                "could not cancel abandoned inline actions because no Tokio runtime was active"
            );
        }
    }
}

/// One capability exposed to a managed application sandbox for exactly one
/// active harness turn. The random bearer is not a Temps API token: it can be
/// used only with this registry entry, whose executor already captures the
/// initiating user's `AuthContext`, application/project scope, and write
/// proposal policy.
#[derive(Clone)]
struct HarnessMcpEntry {
    bearer: String,
    principal_id: i32,
    tools: Arc<Vec<ChatTool>>,
    executor: ToolExecutor,
    interactions: temps_ai::InteractionExecutor,
    tool_slot: Arc<Semaphore>,
    expires_at: Instant,
}

/// Removes a turn capability even when the provider task is cancelled or
/// times out. Nothing is persisted, so a server restart also invalidates every
/// outstanding sandbox capability.
struct HarnessMcpGuard {
    registry: Arc<Mutex<HashMap<String, HarnessMcpEntry>>>,
    bridge_id: String,
}

impl Drop for HarnessMcpGuard {
    fn drop(&mut self) {
        let mut registry = match self.registry.lock() {
            Ok(registry) => registry,
            Err(poisoned) => poisoned.into_inner(),
        };
        registry.remove(&self.bridge_id);
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HarnessMcpError {
    #[error("sandbox tool capability is not available")]
    NotFound,
    #[error("sandbox tool capability is not authorized")]
    Unauthorized,
    #[error("sandbox tool capability has expired")]
    Expired,
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
use temps_auth::context::AuthContext;
use temps_entities::{ai_conversations, ai_messages};

use temps_ai_api_tools::{
    ApiCallScope, ProjectSelectorScope, WriteApiToolsHandle, WritePrepareOutcome,
};

use crate::audit::{AiActionConfirmedAudit, AiActionRejectedAudit, AiActionTransitionFailedAudit};
use crate::pending_actions::PendingActionService;
use crate::provider::ConversationContextProvider;
use crate::sensitive::{redact_json_string, redact_text, redact_value};
use crate::ChatError;

/// Tool name for the write-proposal (confirm-gated) tool.
const TEMPS_WRITE_TOOL_NAME: &str = "temps_write";

fn is_temps_write_tool_name(name: &str) -> bool {
    matches!(name, TEMPS_WRITE_TOOL_NAME | "mcp__temps-chat__temps_write")
}

/// Client-visible tool results must not contain raw data fetched through
/// server-side credentials. The model keeps the full result for reasoning;
/// the live stream and persisted transcript receive only this safe status.
fn public_tool_result(name: &str, result: &str) -> String {
    if is_temps_write_tool_name(name) {
        return redact_json_string(result);
    }

    "Tool completed; detailed result is withheld from the chat transcript.".to_string()
}

/// A proposal exists only when the write tool returned a receipt during this
/// turn. Model prose is not authority: accepting an older receipt or a sentence
/// that merely says "Proposal staged" would strand the user without a durable
/// action or confirmation card.
fn has_fresh_proposal_receipt(tools: &[serde_json::Value]) -> bool {
    tools.iter().any(|tool| {
        let Some(name) = tool.get("name").and_then(serde_json::Value::as_str) else {
            return false;
        };
        if !is_temps_write_tool_name(name) {
            return false;
        }
        let Some(result) = tool.get("result").and_then(serde_json::Value::as_str) else {
            return false;
        };
        serde_json::from_str::<serde_json::Value>(result)
            .ok()
            .and_then(|receipt| {
                receipt
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .map(|status| matches!(status, "proposed" | "proposed_plan"))
            })
            .unwrap_or(false)
    })
}

/// Detect only direct assertions, not explanatory prose that happens to quote
/// the phrase. This keeps the postcondition narrow while covering the wording
/// used by the write-tool contract and common harness responses.
fn claims_proposal_was_staged(content: &str) -> bool {
    let claim = content
        .trim_start_matches(|ch: char| {
            ch.is_whitespace() || matches!(ch, '#' | '*' | '_' | '`' | '>')
        })
        .to_ascii_lowercase();
    [
        "proposal staged",
        "proposal has been staged",
        "the proposal is staged",
        "i staged the proposal",
        "i've staged the proposal",
    ]
    .iter()
    .any(|prefix| claim.starts_with(prefix))
}

/// System prompt for the one-shot title generator. Kept terse so even small
/// local models return a clean label rather than a sentence.
const TITLE_SYSTEM_PROMPT: &str = "You write a short title for a chat based on the user's first message. \
Reply with ONLY the title: 3–6 words, Title Case, no quotes, no surrounding punctuation, no explanation.";

/// Maximum stored title length (chars). Long titles are truncated, not rejected.
const TITLE_MAX_CHARS: usize = 60;

pub(crate) struct ConversationRuntime {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) thinking_level: Option<String>,
    pub(crate) permission_mode: String,
}

/// `default` is a UI/protocol sentinel meaning "let the harness choose". It
/// is not a reasoning variant and must never be validated or passed to a CLI.
fn normalize_thinking_level(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty() && *value != "default")
}

fn permission_mode_auto_approves_provider_tools(permission_mode: &str) -> bool {
    matches!(permission_mode, "auto" | "full-access")
}

fn platform_request_is_destructive(request: &PermissionRequest) -> bool {
    if request
        .input
        .get("method")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|method| method.eq_ignore_ascii_case("DELETE"))
    {
        return true;
    }
    request
        .input
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|steps| {
            steps.iter().any(|step| {
                step.get("method")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|method| method.eq_ignore_ascii_case("DELETE"))
            })
        })
}

fn automatic_platform_decision(request: &PermissionRequest) -> Option<PermissionDecision> {
    if platform_request_is_destructive(request) {
        return None;
    }
    match request.kind {
        PermissionKind::ToolApproval => Some(PermissionDecision::AllowTool),
        PermissionKind::PlanApproval => Some(PermissionDecision::ApprovePlan),
        PermissionKind::Question => None,
    }
}

fn automatic_platform_entry_decision(entry: &PendingPermissionEntry) -> Option<PermissionDecision> {
    automatic_platform_decision(&PermissionRequest {
        id: String::new(),
        kind: entry.kind.clone(),
        tool_name: entry.tool_name.clone(),
        input: entry.input.clone(),
    })
}

fn automatic_pending_decision(entry: &PendingPermissionEntry) -> Option<PermissionDecision> {
    match entry.origin {
        PendingPermissionOrigin::Provider => {
            (entry.kind == PermissionKind::ToolApproval).then_some(PermissionDecision::AllowTool)
        }
        PendingPermissionOrigin::PlatformWrite => automatic_platform_entry_decision(entry),
    }
}

fn cli_session_after_model_change(
    current_model: &str,
    next_model: &str,
    current_session_id: Option<&str>,
) -> Option<String> {
    (current_model == next_model)
        .then(|| current_session_id.map(str::to_string))
        .flatten()
}

fn cli_session_fingerprint_after_model_change(
    current_model: &str,
    next_model: &str,
    current_fingerprint: Option<&str>,
) -> Option<String> {
    (current_model == next_model)
        .then(|| current_fingerprint.map(str::to_string))
        .flatten()
}

/// Provider CLIs keep resumable transcripts in the sandbox home directory.
/// A durable Temps conversation can outlive that provider-local cache (for
/// example after importing an older application workspace or replacing a
/// sandbox volume). Only retry without `--resume` when the provider explicitly
/// says the requested session is missing; authentication, model, and generic
/// process failures must remain visible instead of being hidden by a retry.
fn provider_resume_session_is_missing(provider: &str, reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    match provider {
        "claude_cli" => reason.contains("no conversation found with session id"),
        "codex_cli" => {
            reason.contains("session not found")
                || reason.contains("thread not found")
                || reason.contains("no rollout found")
        }
        "opencode" => reason.contains("session not found") || reason.contains("unknown session"),
        _ => false,
    }
}

async fn clear_missing_provider_session(
    db: &DatabaseConnection,
    conversation_id: i64,
) -> Result<(), sea_orm::DbErr> {
    ai_conversations::Entity::update_many()
        .filter(ai_conversations::Column::Id.eq(conversation_id))
        .col_expr(
            ai_conversations::Column::CliSessionId,
            Expr::value(None::<String>),
        )
        .col_expr(
            ai_conversations::Column::CliSessionFingerprint,
            Expr::value(None::<String>),
        )
        .exec(db)
        .await?;
    Ok(())
}

/// Normalise a model-generated title: take the first non-empty line, strip
/// wrapping quotes and trailing punctuation, collapse whitespace, and cap the
/// length. Reasoning models sometimes prepend stray lines, so we defensively
/// keep only the first meaningful one.
fn clean_title(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim_end_matches(['.', '!', '?', ':', ',', ';'])
        .trim();
    if trimmed.chars().count() > TITLE_MAX_CHARS {
        trimmed
            .chars()
            .take(TITLE_MAX_CHARS)
            .collect::<String>()
            .trim_end()
            .to_string()
    } else {
        trimmed.to_string()
    }
}

/// Ask the AI for a concise title for `first_message` and store it on the
/// conversation. Fire-and-forget: every failure (AI unavailable, empty result,
/// DB error) is swallowed with a debug log so it can never break the chat.
///
/// Uses [`AiService::complete`] rather than `chat()` — it needs no tools, and
/// `complete()` is the workload [`temps_ai_agent_cli::DispatchingAiService`]
/// routes to whichever provider is active, so a subscription agent CLI still
/// gets a real title instead of always falling back to the generic seed
/// title (`chat()` stays gateway-only unconditionally; using it here would
/// silently fail for every CLI-backed instance).
async fn generate_and_store_title(
    ai: &Arc<dyn AiService>,
    db: &Arc<DatabaseConnection>,
    conv_id: i64,
    project_id: Option<i32>,
    provider: String,
    model: String,
    first_message: &str,
) {
    let req = AiRequest {
        purpose: "chat.title".to_string(),
        project_id,
        provider: Some(provider),
        model: Some(model),
        system: Some(TITLE_SYSTEM_PROMPT.to_string()),
        prompt: format!("First message:\n{first_message}\n\nTitle:"),
        ..Default::default()
    };
    let raw = match ai.complete(req).await {
        Ok(resp) => resp.text,
        Err(e) => {
            tracing::debug!("chat title generation failed for conv {conv_id}: {e}");
            return;
        }
    };
    let title = clean_title(&raw);
    if title.is_empty() {
        tracing::debug!("chat title generation produced an empty title for conv {conv_id}");
        return;
    }
    let am = ai_conversations::ActiveModel {
        id: Set(conv_id),
        title: Set(Some(title)),
        ..Default::default()
    };
    if let Err(e) = am.update(db.as_ref()).await {
        tracing::debug!("failed to store generated title for conv {conv_id}: {e}");
    }
}

/// One item in the live `send_message` stream. The plain-text path yields only
/// `Token`s; the agentic tool loop additionally surfaces each tool invocation
/// (`ToolCall`, emitted just before the tool runs) and its outcome
/// (`ToolResult`, emitted right after), so the client can render tool activity
/// in real time. Only the final assistant text is persisted; tool events are
/// live immediately and persisted into the completed assistant message.
// `Eq` is intentionally absent: `PermissionRequested.input` is `serde_json::Value`
// which implements `PartialEq` but not `Eq` (NaN-unsafe float comparison).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatStreamEvent {
    /// A chunk of assistant prose to append to the message content.
    Token(String),
    /// The model is about to invoke a tool. `arguments` is the raw JSON-args
    /// string the model emitted.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    /// A tool finished; `content` is the string it returned.
    ToolResult {
        id: String,
        name: String,
        content: String,
    },
    /// The active provider is waiting for the user to approve or deny a
    /// tool/question/plan. The SSE
    /// handler emits this as a `permission_requested` event so the UI can
    /// render the appropriate card.  The user resolves it via
    /// `POST .../permissions/{id}/resolve`, which unblocks the subprocess.
    PermissionRequested {
        id: String,
        kind: PermissionKind,
        tool_name: String,
        input: serde_json::Value,
    },
}

/// A conversation plus its project's display info, for the unified switcher.
pub struct ConversationWithProject {
    pub conversation: ai_conversations::Model,
    pub project_name: Option<String>,
    pub project_slug: Option<String>,
}

/// Domain result used by the readiness HTTP adapter and other callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatReadiness {
    pub ai_configured: bool,
}

/// One bounded page of client-visible conversation history.
///
/// Messages are always returned oldest-first so a client can prepend an older
/// page without reordering it. `next_before` is deliberately opaque to clients;
/// they should return it unchanged when requesting the preceding page.
#[derive(Debug, Clone, PartialEq)]
pub struct MessagePage {
    pub messages: Vec<ai_messages::Model>,
    pub has_more: bool,
    pub next_before: Option<String>,
}

/// Validation failure for an opaque message-history cursor.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MessageCursorError {
    #[error("message cursor '{cursor}' does not use the supported m1 format")]
    InvalidFormat { cursor: String },
    #[error("message cursor '{cursor}' does not contain a positive message id")]
    InvalidMessageId { cursor: String },
}

const MESSAGE_CURSOR_PREFIX: &str = "m1_";

pub fn encode_message_before_cursor(message_id: i64) -> String {
    format!("{MESSAGE_CURSOR_PREFIX}{message_id:x}")
}

/// Decode a cursor previously returned by [`MessagePage::next_before`].
///
/// Keeping this parser beside the query contract lets HTTP adapters reject
/// malformed cursors before calling [`ConversationService::messages_page`]
/// without exposing the underlying row id as part of the public API.
pub fn decode_message_before_cursor(cursor: &str) -> Result<i64, MessageCursorError> {
    let encoded_id = cursor.strip_prefix(MESSAGE_CURSOR_PREFIX).ok_or_else(|| {
        MessageCursorError::InvalidFormat {
            cursor: cursor.to_string(),
        }
    })?;
    let message_id =
        i64::from_str_radix(encoded_id, 16).map_err(|_| MessageCursorError::InvalidMessageId {
            cursor: cursor.to_string(),
        })?;
    if message_id <= 0 {
        return Err(MessageCursorError::InvalidMessageId {
            cursor: cursor.to_string(),
        });
    }
    Ok(message_id)
}

/// Optional write-tool support wired into a `ConversationService` via
/// [`ConversationService::with_write_support`].
struct WriteSupport {
    write_handle: Arc<WriteApiToolsHandle>,
    pending: Arc<PendingActionService>,
    audit: Arc<dyn AuditLogger>,
}

/// One server-owned turn currently executing for a conversation.
struct ActiveTurn {
    turn_id: String,
    abort: AbortHandle,
    /// The CLI launch mode is fixed, but this server-owned flag can safely
    /// elevate later sandbox tool requests from "ask" to "auto" mid-turn.
    auto_approve_provider_tools: Arc<AtomicBool>,
}

/// Owns conversation persistence + AI turn streaming. Construct once with the
/// registered context providers; resolve via the plugin DI.
pub struct ConversationService {
    db: Arc<DatabaseConnection>,
    ai: Arc<dyn AiService>,
    providers: HashMap<&'static str, Arc<dyn ConversationContextProvider>>,
    /// Optional write-tool wiring. `None` until
    /// [`ConversationService::with_write_support`] is called.
    write_support: Option<WriteSupport>,
    /// Reads operator-tunable chat limits. Consulted once per turn (the service
    /// caches, so this is not a per-turn database hit) rather than at startup,
    /// so changing the timeout in Settings takes effect on the next message
    /// instead of requiring a restart. `None` in tests and in any wiring that
    /// has not supplied it — the compiled default applies.
    config: Option<Arc<temps_config::ConfigService>>,
    /// The trusted builder for a durable application workspace. Only
    /// application threads receive a workspace; gateway conversations remain
    /// data-only and never gain filesystem execution.
    application_workspaces: Option<Arc<crate::ApplicationWorkspaceService>>,
    /// Creates a first-class sandbox row for an application workspace. The
    /// row gives a harness a stable opaque preview identity; without it a raw
    /// Docker label cannot safely be turned into a browser URL.
    application_sandboxes: Option<Arc<temps_sandbox::SandboxService>>,
    /// Shared application topology/resource authority. Production wiring uses
    /// the same instance as HTTP handlers so admission checks and topology
    /// mutations participate in one in-process serialization boundary.
    application_service: Option<Arc<crate::ApplicationService>>,
    /// In-process registry for normalized provider interaction requests.
    ///
    /// Keyed by the CLI's own `request_id` (a UUID).  The resolve endpoint
    /// claims an entry by removing it (atomic remove-to-claim prevents double
    /// resolution: the loser of the race gets a `None` back → 409 Conflict).
    ///
    /// Each entry carries the sender, the conversation's `public_id`, and the
    /// permission `kind` so that `resolve_permission` can:
    ///   (a) reject attempts to resolve a permission through a different
    ///       conversation's URL (IDOR guard), and
    ///   (b) reject a `PermissionDecision` variant that is incompatible with
    ///       the kind the CLI actually requested.
    ///
    /// Entries live only for the duration of one `control_request` / await
    /// cycle inside the provider turn task. If the provider exits
    /// before resolution, the send fails (receiver dropped) and the task
    /// auto-denies — so no entry can be orphaned indefinitely.
    ///
    /// Plain `std::sync::Mutex` (not tokio's): the critical section is tiny
    /// (insert / remove on a HashMap, no I/O), so the sync mutex is the right
    /// choice here — using tokio's async mutex for a non-async lock site is
    /// unnecessary overhead.
    pub pending_permissions: Arc<Mutex<HashMap<String, PendingPermissionEntry>>>,
    /// Per-conversation fan-out for cross-tab live sync. Keyed by
    /// `conversation.id`, lazily created on first subscriber or first publish.
    /// A second browser tab watching the same conversation subscribes here
    /// (via the `GET .../stream` WebSocket) to see the same events the
    /// sending tab receives over its own SSE response — without this, a
    /// turn started in one tab is invisible everywhere else until a manual
    /// reload. Single-node only: AI chat never runs on worker nodes, so a
    /// plain in-process `tokio::sync::broadcast` is sufficient (same
    /// reasoning as `temps-routes`' route-reload subscriber).
    conversation_broadcasts: Arc<Mutex<HashMap<i64, broadcast::Sender<WireEvent>>>>,
    /// Running harness/provider tasks. Browser connections only subscribe to
    /// their output; they never own task lifetime. Explicit Stop uses this
    /// registry to cancel the matching server turn.
    active_turns: Arc<Mutex<HashMap<i64, ActiveTurn>>>,
    /// Ephemeral MCP capabilities used only by managed application sandboxes.
    /// Entries are random, user-scoped, turn-owned, and never persisted.
    harness_mcp_entries: Arc<Mutex<HashMap<String, HarnessMcpEntry>>>,
}

/// One event on a conversation's live wire. The server-owned producer derives
/// this and the detachable SSE item from the same normalized event.
#[derive(Debug, Clone)]
pub struct WireEvent {
    /// SSE/WS event name, e.g. `"token"` (implicit/unnamed for plain text in
    /// SSE), `"tool_call"`, `"permission_requested"`, `"user_message"`.
    pub event: String,
    /// The JSON (or plain text, for token deltas) payload.
    pub data: String,
}

/// Convert one normalized provider event into the shared live-wire contract.
/// The producer publishes here, before attempting delivery to any particular
/// SSE viewer, so closing or refreshing a browser cannot silence other tabs.
fn wire_event_for(item: &Result<ChatStreamEvent, ChatError>) -> WireEvent {
    match item {
        Ok(ChatStreamEvent::Token(text)) => WireEvent {
            event: "token".to_string(),
            data: text.clone(),
        },
        Ok(ChatStreamEvent::ToolCall {
            id,
            name,
            arguments,
        }) => WireEvent {
            event: "tool_call".to_string(),
            data: serde_json::json!({
                "id": id,
                "name": name,
                "arguments": arguments,
            })
            .to_string(),
        },
        Ok(ChatStreamEvent::ToolResult { id, name, content }) => WireEvent {
            event: "tool_result".to_string(),
            data: serde_json::json!({
                "id": id,
                "name": name,
                "content": content,
            })
            .to_string(),
        },
        Ok(ChatStreamEvent::PermissionRequested {
            id,
            kind,
            tool_name,
            input,
        }) => WireEvent {
            event: "permission_requested".to_string(),
            data: serde_json::json!({
                "id": id,
                "kind": kind,
                "tool_name": tool_name,
                "input": input,
            })
            .to_string(),
        },
        Err(error) => WireEvent {
            event: "error".to_string(),
            data: {
                let failure = error.public_failure();
                serde_json::json!({
                    "code": failure.code,
                    "title": failure.title,
                    "detail": failure.detail,
                    "retryable": failure.retryable,
                })
                .to_string()
            },
        },
    }
}

fn emit_turn_event(
    tx: &tokio::sync::mpsc::UnboundedSender<Result<ChatStreamEvent, ChatError>>,
    live: &broadcast::Sender<WireEvent>,
    item: Result<ChatStreamEvent, ChatError>,
) {
    let _ = live.send(wire_event_for(&item));
    // Zero SSE viewers is normal after a refresh. Execution and WebSocket
    // publication remain server-owned and continue until terminal state.
    let _ = tx.send(item);
}

fn assistant_message_metadata(
    tools: &[serde_json::Value],
    parts: &[serde_json::Value],
    draft: bool,
) -> Option<serde_json::Value> {
    let mut metadata = serde_json::Map::new();
    if !tools.is_empty() {
        metadata.insert(
            "tools".to_string(),
            serde_json::Value::Array(tools.to_vec()),
        );
    }
    if !parts.is_empty() {
        metadata.insert(
            "parts".to_string(),
            serde_json::Value::Array(parts.to_vec()),
        );
    }
    if draft {
        metadata.insert("draft".to_string(), serde_json::Value::Bool(true));
    }
    (!metadata.is_empty()).then_some(serde_json::Value::Object(metadata))
}

async fn persist_assistant_message(
    db: &DatabaseConnection,
    message_id: i64,
    content: &str,
    tools: &[serde_json::Value],
    completed_parts: &[serde_json::Value],
    open_text: &str,
    draft: bool,
) -> Result<(), sea_orm::DbErr> {
    let mut parts = completed_parts.to_vec();
    if !open_text.is_empty() {
        parts.push(serde_json::json!({ "type": "text", "text": open_text }));
    }
    ai_messages::Entity::update_many()
        .filter(ai_messages::Column::Id.eq(message_id))
        .col_expr(
            ai_messages::Column::Content,
            Expr::value(content.to_string()),
        )
        .col_expr(
            ai_messages::Column::Metadata,
            Expr::value(assistant_message_metadata(tools, &parts, draft)),
        )
        .exec(db)
        .await?;
    Ok(())
}

fn record_tool_call(
    tools: &mut Vec<serde_json::Value>,
    parts: &mut Vec<serde_json::Value>,
    id: &str,
    name: &str,
    arguments: &str,
) {
    if tools
        .iter()
        .any(|tool| tool.get("id").and_then(serde_json::Value::as_str) == Some(id))
    {
        return;
    }
    let tool = serde_json::json!({
        "id": id,
        "name": name,
        "arguments": arguments,
        "result": serde_json::Value::Null,
    });
    tools.push(tool.clone());
    parts.push(serde_json::json!({ "type": "tool", "tool": tool }));
}

fn record_tool_result(
    tools: &mut Vec<serde_json::Value>,
    parts: &mut Vec<serde_json::Value>,
    id: &str,
    name: &str,
    arguments: &str,
    result: &str,
) {
    record_tool_call(tools, parts, id, name, arguments);
    for tool in tools.iter_mut() {
        if tool.get("id").and_then(serde_json::Value::as_str) == Some(id) {
            tool["result"] = serde_json::Value::String(result.to_string());
        }
    }
    for part in parts.iter_mut() {
        let Some(tool) = part.get_mut("tool") else {
            continue;
        };
        if tool.get("id").and_then(serde_json::Value::as_str) == Some(id) {
            tool["result"] = serde_json::Value::String(result.to_string());
        }
    }
}

/// Bounded broadcast capacity per conversation. Sized for a burst of tool
/// calls/tokens while a tab is briefly backgrounded; a subscriber that falls
/// further behind than this gets an explicit `resync_required` frame
/// (`RecvError::Lagged`) rather than silently missing history.
const CONVERSATION_BROADCAST_CAPACITY: usize = 256;

impl ConversationService {
    /// Upper bound on rows returned by the global switcher, so the response (and
    /// the in-memory toggle filter that follows) can't grow without limit.
    const LIST_ALL_LIMIT: u64 = 200;

    pub fn new(
        db: Arc<DatabaseConnection>,
        ai: Arc<dyn AiService>,
        providers: Vec<Arc<dyn ConversationContextProvider>>,
    ) -> Self {
        let providers = providers
            .into_iter()
            .map(|p| (p.context_type(), p))
            .collect();
        Self {
            db,
            ai,
            providers,
            write_support: None,
            config: None,
            application_workspaces: None,
            application_sandboxes: None,
            application_service: None,
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(Mutex::new(HashMap::new())),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            harness_mcp_entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn register_harness_mcp(
        registry: Arc<Mutex<HashMap<String, HarnessMcpEntry>>>,
        internal_api_url: &str,
        principal_id: i32,
        tools: Vec<ChatTool>,
        executor: ToolExecutor,
        interactions: temps_ai::InteractionExecutor,
        lifetime: Duration,
    ) -> (HarnessMcpServer, HarnessMcpGuard) {
        let bridge_id = uuid::Uuid::new_v4().simple().to_string();
        let bearer = format!(
            "tmcp_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let entry = HarnessMcpEntry {
            bearer: bearer.clone(),
            principal_id,
            tools: Arc::new(tools),
            executor,
            interactions,
            tool_slot: Arc::new(Semaphore::new(1)),
            expires_at: Instant::now() + lifetime,
        };
        match registry.lock() {
            Ok(mut registry) => {
                registry.insert(bridge_id.clone(), entry);
            }
            Err(poisoned) => {
                poisoned.into_inner().insert(bridge_id.clone(), entry);
            }
        }
        let server = HarnessMcpServer {
            url: format!(
                "{}/api/ai/sandbox-tools/{bridge_id}/mcp",
                internal_api_url.trim_end_matches('/')
            ),
            authorization_token: bearer,
        };
        let guard = HarnessMcpGuard {
            registry,
            bridge_id,
        };
        (server, guard)
    }

    /// Handle one MCP JSON-RPC request from a managed application sandbox.
    ///
    /// This route does not accept normal user/API credentials. Its bearer is a
    /// one-turn capability whose executor was built from the authenticated
    /// browser request. Consequently the model can neither widen the project
    /// scope nor keep using the capability after the turn completes.
    pub async fn handle_harness_mcp_request(
        &self,
        bridge_id: &str,
        bearer: &str,
        request: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, HarnessMcpError> {
        let entry = {
            let mut registry = match self.harness_mcp_entries.lock() {
                Ok(registry) => registry,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(entry) = registry.get(bridge_id).cloned() else {
                return Err(HarnessMcpError::NotFound);
            };
            if entry.expires_at <= Instant::now() {
                registry.remove(bridge_id);
                return Err(HarnessMcpError::Expired);
            }
            entry
        };
        if !constant_time_eq(entry.bearer.as_bytes(), bearer.as_bytes()) {
            return Err(HarnessMcpError::Unauthorized);
        }
        tracing::debug!(
            principal_id = entry.principal_id,
            bridge_id,
            "authorized application sandbox platform-tool request"
        );

        // MCP notifications have no response body.
        let Some(id) = request.get("id").cloned() else {
            return Ok(None);
        };
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "temps-application", "version": "1"}
                }
            }),
            "tools/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": entry.tools.iter().map(|tool| serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.parameters,
                })).chain(std::iter::once(serde_json::json!({
                    "name": "temps_native_permission",
                    "description": "Internal approval bridge used by the development harness. Do not invoke directly.",
                    "inputSchema": {
                        "type": "object",
                        "required": ["tool_name", "input"],
                        "properties": {
                            "tool_name": {"type": "string"},
                            "input": {"type": "object", "additionalProperties": true}
                        },
                        "additionalProperties": true
                    }
                }))).collect::<Vec<_>>()}
            }),
            "ping" => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or_default();
                let name = params
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if name == "temps_native_permission" {
                    let arguments = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let tool_name = arguments
                        .get("tool_name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Unknown tool")
                        .to_string();
                    let input = arguments
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let permission = temps_ai::PermissionRequest {
                        id: uuid::Uuid::new_v4().simple().to_string(),
                        kind: temps_ai::PermissionKind::ToolApproval,
                        tool_name,
                        input: input.clone(),
                    };
                    let remaining = entry.expires_at.saturating_duration_since(Instant::now());
                    let decision =
                        tokio::time::timeout(remaining, (entry.interactions)(permission)).await;
                    let payload = match decision {
                        Ok(Ok(temps_ai::PermissionDecision::AllowTool)) => serde_json::json!({
                            "behavior": "allow",
                            "updatedInput": input,
                        }),
                        Ok(Ok(temps_ai::PermissionDecision::DenyTool { reason })) => {
                            serde_json::json!({
                                "behavior": "deny",
                                "message": reason.unwrap_or_else(|| "Permission denied".to_string()),
                            })
                        }
                        Ok(Ok(_)) => serde_json::json!({
                            "behavior": "deny",
                            "message": "The approval response did not match this tool request",
                        }),
                        Ok(Err(error)) => serde_json::json!({
                            "behavior": "deny",
                            "message": error.to_string(),
                        }),
                        Err(_) => serde_json::json!({
                            "behavior": "deny",
                            "message": "Permission request timed out",
                        }),
                    };
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": payload.to_string()}],
                            "isError": false,
                        }
                    })
                } else if !entry.tools.iter().any(|tool| tool.name == name) {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": "Tool is not available for this application turn"}],
                            "isError": true,
                        }
                    })
                } else {
                    let permit = match entry.tool_slot.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            return Ok(Some(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "content": [{"type": "text", "text": "Another Temps platform tool is already running for this turn"}],
                                    "isError": true,
                                }
                            })));
                        }
                    };
                    let call = ToolCall {
                        id: uuid::Uuid::new_v4().simple().to_string(),
                        name: name.to_string(),
                        arguments: params
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}))
                            .to_string(),
                    };
                    // A platform write may be waiting on a human approval. Do
                    // not impose the old 30-second read-tool timeout on that
                    // interaction; keep it bounded by the turn capability's
                    // own lifetime instead.
                    let remaining = entry.expires_at.saturating_duration_since(Instant::now());
                    let result = tokio::time::timeout(remaining, (entry.executor)(call)).await;
                    drop(permit);
                    let (text, is_error) = match result {
                        Err(_) => (
                            "Temps platform tool approval or execution timed out with the active turn"
                                .to_string(),
                            true,
                        ),
                        Ok(Ok(text)) => (text, false),
                        Ok(Err(error)) => (error.to_string(), true),
                    };
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": text}],
                            "isError": is_error,
                        }
                    })
                }
            }
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "Method not found"},
            }),
        };
        Ok(Some(response))
    }

    /// Get-or-create the broadcast sender for a conversation's live wire
    /// (cross-tab sync). Cheap: only touches the registry, never I/O.
    fn broadcast_sender_for(&self, conv_id: i64) -> broadcast::Sender<WireEvent> {
        let mut map = match self.conversation_broadcasts.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        map.entry(conv_id)
            .or_insert_with(|| broadcast::channel(CONVERSATION_BROADCAST_CAPACITY).0)
            .clone()
    }

    /// Subscribe to a conversation's live wire (used by the `GET .../stream`
    /// WebSocket handler). Public so `handlers.rs` never touches the
    /// registry lock directly.
    pub fn subscribe_conversation(&self, conv_id: i64) -> broadcast::Receiver<WireEvent> {
        self.broadcast_sender_for(conv_id).subscribe()
    }

    /// Publish one wire event to every subscriber of a conversation.
    /// Best-effort: `send` only errors when there are zero subscribers, which
    /// is the common case (no tab open) and not a failure. Durable state remains
    /// authoritative and clients resync it after reconnecting.
    pub fn publish_wire_event(
        &self,
        conv_id: i64,
        event: impl Into<String>,
        data: impl Into<String>,
    ) {
        let _ = self.broadcast_sender_for(conv_id).send(WireEvent {
            event: event.into(),
            data: data.into(),
        });
    }

    /// Mark turns left `running` by a previous server process as interrupted.
    /// A browser refresh does not invoke this; only process startup does, when
    /// no in-memory task can still own those rows.
    pub async fn recover_interrupted_turns(&self) -> Result<u64, ChatError> {
        let result = ai_conversations::Entity::update_many()
            .filter(ai_conversations::Column::TurnStatus.eq("running"))
            .col_expr(
                ai_conversations::Column::TurnStatus,
                Expr::value("interrupted"),
            )
            .col_expr(
                ai_conversations::Column::ActiveTurnId,
                Expr::value(Option::<String>::None),
            )
            .col_expr(
                ai_conversations::Column::TurnStartedAt,
                Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected)
    }

    /// Atomically claim the conversation for one idempotent turn. This is the
    /// server-side concurrency boundary: two tabs cannot append duplicate user
    /// messages or start two harnesses for the same conversation.
    pub async fn claim_turn(
        &self,
        conversation: &ai_conversations::Model,
        turn_id: &str,
    ) -> Result<chrono::DateTime<Utc>, ChatError> {
        let turn_started_at = Utc::now();
        let result = ai_conversations::Entity::update_many()
            .filter(ai_conversations::Column::Id.eq(conversation.id))
            .filter(ai_conversations::Column::TurnStatus.ne("running"))
            .filter(
                Condition::any()
                    .add(ai_conversations::Column::LastTurnId.is_null())
                    .add(ai_conversations::Column::LastTurnId.ne(turn_id)),
            )
            .col_expr(ai_conversations::Column::TurnStatus, Expr::value("running"))
            .col_expr(
                ai_conversations::Column::ActiveTurnId,
                Expr::value(Some(turn_id.to_string())),
            )
            .col_expr(
                ai_conversations::Column::LastTurnId,
                Expr::value(Some(turn_id.to_string())),
            )
            .col_expr(
                ai_conversations::Column::TurnStartedAt,
                Expr::value(Some(turn_started_at)),
            )
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 1 {
            return Ok(turn_started_at);
        }

        let current = ai_conversations::Entity::find_by_id(conversation.id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ChatError::NotFound(conversation.public_id.clone()))?;
        if current.last_turn_id.as_deref() == Some(turn_id) {
            Err(ChatError::DuplicateTurn {
                conversation_id: conversation.public_id.clone(),
                turn_id: turn_id.to_string(),
            })
        } else {
            Err(ChatError::TurnInProgress {
                conversation_id: conversation.public_id.clone(),
            })
        }
    }

    /// Release a claimed turn only if the opaque id still owns the row. The
    /// conditional update prevents a late completion from clearing a newer
    /// turn that started after an explicit cancellation.
    pub async fn finish_turn(
        &self,
        conversation_id: i64,
        turn_id: &str,
        status: &str,
    ) -> Result<bool, ChatError> {
        let result = ai_conversations::Entity::update_many()
            .filter(ai_conversations::Column::Id.eq(conversation_id))
            .filter(ai_conversations::Column::ActiveTurnId.eq(turn_id))
            .col_expr(
                ai_conversations::Column::TurnStatus,
                Expr::value(status.to_string()),
            )
            .col_expr(
                ai_conversations::Column::ActiveTurnId,
                Expr::value(Option::<String>::None),
            )
            .col_expr(
                ai_conversations::Column::TurnStartedAt,
                Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected == 1)
    }

    /// Cancel the active provider task explicitly. Disconnecting an SSE/WS
    /// viewer never calls this; only the authenticated Stop endpoint does.
    pub async fn cancel_turn(
        &self,
        conversation: &ai_conversations::Model,
    ) -> Result<bool, ChatError> {
        let active = match self.active_turns.lock() {
            Ok(mut turns) => turns.remove(&conversation.id),
            Err(poisoned) => poisoned.into_inner().remove(&conversation.id),
        };
        let turn_id = active
            .as_ref()
            .map(|turn| turn.turn_id.clone())
            .or_else(|| conversation.active_turn_id.clone());

        let Some(turn_id) = turn_id else {
            // A malformed legacy row can say `running` without an owner id.
            // Clear only that exact state so a concurrently claimed real turn
            // can never be cancelled by this recovery path.
            if conversation.turn_status != "running" {
                return Ok(false);
            }
            let result = ai_conversations::Entity::update_many()
                .filter(ai_conversations::Column::Id.eq(conversation.id))
                .filter(ai_conversations::Column::TurnStatus.eq("running"))
                .filter(ai_conversations::Column::ActiveTurnId.is_null())
                .col_expr(
                    ai_conversations::Column::TurnStatus,
                    Expr::value("cancelled"),
                )
                .col_expr(
                    ai_conversations::Column::TurnStartedAt,
                    Expr::value(Option::<chrono::DateTime<Utc>>::None),
                )
                .exec(self.db.as_ref())
                .await?;
            if result.rows_affected == 1 {
                self.publish_wire_event(conversation.id, "turn_complete", "");
                return Ok(true);
            }
            return Ok(false);
        };

        if let Some(active) = active {
            active.abort.abort();
        }
        let cancelled = self
            .finish_turn(conversation.id, &turn_id, "cancelled")
            .await?;
        if cancelled {
            self.publish_wire_event(conversation.id, "turn_complete", "");
        }
        Ok(cancelled)
    }

    /// Supply the settings service so operator-tuned chat limits apply.
    ///
    /// Optional: without it the compiled defaults are used, so a minimal wiring
    /// (and every test) still works without standing up config.
    pub fn with_config(mut self, config: Arc<temps_config::ConfigService>) -> Self {
        self.config = Some(config);
        self
    }

    /// Supply the instance-owned workspace builder used by application
    /// harnesses. Keeping this optional preserves minimal/test composition
    /// while production wiring fails closed if an application thread somehow
    /// reaches a service without the builder.
    pub fn with_application_workspaces(
        mut self,
        workspaces: Arc<crate::ApplicationWorkspaceService>,
    ) -> Self {
        self.application_workspaces = Some(workspaces);
        self
    }

    pub fn with_application_sandboxes(
        mut self,
        sandboxes: Arc<temps_sandbox::SandboxService>,
    ) -> Self {
        self.application_sandboxes = Some(sandboxes);
        self
    }

    pub fn with_application_service(
        mut self,
        applications: Arc<crate::ApplicationService>,
    ) -> Self {
        self.application_service = Some(applications);
        self
    }

    /// Attach write-tool support (the `temps_write` tool + durable action
    /// records and audit sink). This is called by the plugin after service
    /// construction once all three are available.
    ///
    /// When not called, the service degrades gracefully: `temps_write` is not
    /// offered and no pending-action rows are created.
    pub fn with_write_support(
        mut self,
        write_handle: Arc<WriteApiToolsHandle>,
        pending: Arc<PendingActionService>,
        audit: Arc<dyn AuditLogger>,
    ) -> Self {
        self.write_support = Some(WriteSupport {
            write_handle,
            pending,
            audit,
        });
        self
    }

    /// Is the selected AI provider configured? Feature opt-in is checked by
    /// the handler; transport-specific readiness stays behind `AiService`.
    pub async fn ai_available(&self) -> bool {
        self.ai.is_available().await
    }

    /// Is the selected provider ready to serve a tool-calling chat turn?
    ///
    /// Conversations pin a provider at creation time. Checking the ambient
    /// default would reject a healthy host harness whenever the gateway is
    /// deliberately left unconfigured.
    pub async fn ai_available_for(&self, provider: Option<&str>) -> bool {
        self.ai.chat_capable_for(provider).await
    }

    /// Report whether the instance has an AI provider available.
    ///
    /// The project lookup intentionally remains here so callers keep receiving
    /// a typed not-found error. Project access is enforced by the HTTP layer;
    /// there is no separate project-level chat opt-in.
    pub async fn chat_readiness(&self, project_id: i32) -> Result<ChatReadiness, ChatError> {
        temps_entities::projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ChatError::ProjectLookup { project_id, source })?
            .ok_or(ChatError::ProjectNotFound(project_id))?;

        Ok(ChatReadiness {
            ai_configured: self.ai_available().await,
        })
    }

    /// The current creator's active conversation for a context, if one exists.
    pub async fn find_by_context(
        &self,
        project_id: Option<i32>,
        user_id: i32,
        context_type: &str,
        context_id: &str,
    ) -> Result<Option<ai_conversations::Model>, ChatError> {
        Ok(ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::ContextType.eq(context_type))
            .filter(ai_conversations::Column::ContextId.eq(context_id))
            .filter(ai_conversations::Column::Status.eq("active"))
            .one(self.db.as_ref())
            .await?)
    }

    /// Load a conversation by its internal id while retaining project scope.
    /// Used only to authorize child resources such as pending actions.
    pub async fn get_by_id(
        &self,
        project_id: i32,
        user_id: i32,
        conversation_id: i64,
    ) -> Result<ai_conversations::Model, ChatError> {
        ai_conversations::Entity::find_by_id(conversation_id)
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ChatError::NotFound(conversation_id.to_string()))
    }

    /// Load a conversation by internal id and owner, independently of any
    /// optional project context. Used to authorize user-rooted child resources.
    pub async fn get_owned_by_id(
        &self,
        user_id: i32,
        conversation_id: i64,
    ) -> Result<ai_conversations::Model, ChatError> {
        ai_conversations::Entity::find_by_id(conversation_id)
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ChatError::NotFound(conversation_id.to_string()))
    }

    /// One creator's active conversations for a project, most-recently-active first.
    pub async fn list_conversations(
        &self,
        project_id: i32,
        user_id: i32,
    ) -> Result<Vec<ai_conversations::Model>, ChatError> {
        self.list_conversations_with_status(project_id, user_id, "active")
            .await
    }

    /// One creator's conversations for a project in one lifecycle state.
    pub async fn list_conversations_with_status(
        &self,
        project_id: i32,
        user_id: i32,
        status: &str,
    ) -> Result<Vec<ai_conversations::Model>, ChatError> {
        Ok(ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::Status.eq(status))
            .order_by_desc(ai_conversations::Column::LastActivityAt)
            .all(self.db.as_ref())
            .await?)
    }

    /// One creator's active conversations across every project, most-recently-active
    /// first, each annotated with its project's name/slug so the UI can show
    /// where the chat was started and link back to it. Powers the unified
    /// "all chats" switcher.
    ///
    /// Conversations are private to their creator. This protects persisted
    /// tool results and resumable provider sessions from other project members.
    /// The database requires a non-null owner; every query additionally binds
    /// the current user id so another user's public id remains indistinguishable
    /// from a missing conversation.
    ///
    /// Bounded by [`Self::LIST_ALL_LIMIT`] (most-recently-active first) so the
    /// response can't grow unbounded with thread count — a resource-exhaustion
    /// guard. The switcher only needs the recent set; older chats remain
    /// reachable per-project.
    pub async fn list_all_conversations(
        &self,
        user_id: i32,
        hidden_project_ids: &[i32],
    ) -> Result<Vec<ConversationWithProject>, ChatError> {
        self.list_all_conversations_with_status(user_id, hidden_project_ids, "active")
            .await
    }

    /// One creator's conversations across every visible project in one
    /// lifecycle state. The status is server-selected from a closed HTTP enum,
    /// never accepted as an arbitrary database filter.
    pub async fn list_all_conversations_with_status(
        &self,
        user_id: i32,
        hidden_project_ids: &[i32],
        status: &str,
    ) -> Result<Vec<ConversationWithProject>, ChatError> {
        let mut query = ai_conversations::Entity::find()
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::Status.eq(status))
            .order_by_desc(ai_conversations::Column::LastActivityAt)
            .limit(Self::LIST_ALL_LIMIT);
        if !hidden_project_ids.is_empty() {
            query = query.filter(
                Condition::any()
                    .add(ai_conversations::Column::ProjectId.is_null())
                    .add(
                        ai_conversations::Column::ProjectId
                            .is_not_in(hidden_project_ids.iter().copied()),
                    ),
            );
        }
        let convs = query.all(self.db.as_ref()).await?;

        let mut ids: Vec<i32> = convs.iter().filter_map(|c| c.project_id).collect();
        ids.sort_unstable();
        ids.dedup();
        let projects = if ids.is_empty() {
            Vec::new()
        } else {
            temps_entities::projects::Entity::find()
                .filter(temps_entities::projects::Column::Id.is_in(ids))
                .all(self.db.as_ref())
                .await?
        };
        let by_id: HashMap<i32, (String, String)> = projects
            .into_iter()
            .map(|p| (p.id, (p.name, p.slug)))
            .collect();

        Ok(convs
            .into_iter()
            .filter(|conversation| {
                conversation
                    .project_id
                    .is_none_or(|project_id| !hidden_project_ids.contains(&project_id))
            })
            .filter_map(|c| {
                let info = c
                    .project_id
                    .and_then(|project_id| by_id.get(&project_id).cloned());
                // Exclude only conversations whose project is missing. Project
                // membership filtering is supplied by the caller; the legacy
                // per-project chat toggle is deliberately ignored.
                match info {
                    Some((name, slug)) => Some(ConversationWithProject {
                        project_name: Some(name),
                        project_slug: Some(slug),
                        conversation: c,
                    }),
                    None if c.project_id.is_none() => Some(ConversationWithProject {
                        project_name: None,
                        project_slug: None,
                        conversation: c,
                    }),
                    _ => None,
                }
            })
            .collect())
    }

    /// A conversation by public id, scoped to both project and creator.
    pub async fn get_by_public_id(
        &self,
        project_id: i32,
        user_id: i32,
        public_id: &str,
    ) -> Result<ai_conversations::Model, ChatError> {
        ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::PublicId.eq(public_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ChatError::NotFound(public_id.to_string()))
    }

    /// A private conversation by public id and owner. Project/application
    /// context is descriptive and never participates in this ownership check.
    pub async fn get_owned_by_public_id(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<ai_conversations::Model, ChatError> {
        ai_conversations::Entity::find()
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::PublicId.eq(public_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ChatError::NotFound(public_id.to_string()))
    }

    /// The still-unresolved permission request for this conversation, if any
    /// (ADR-038 Phase 2). A page reload only sees the plain-text "asked"
    /// message persisted by `send_message_via_interactive_cli` — without this,
    /// a question that arrived while the tab was away (backgrounded, network
    /// blip, reload) becomes inert text with no way to answer it. At most one
    /// permission is ever pending per conversation (the CLI subprocess blocks
    /// on it before continuing), so the first match is authoritative.
    pub fn pending_permission_for(
        &self,
        conv_public_id: &str,
    ) -> Option<temps_ai::streaming::PermissionRequest> {
        let map = match self.pending_permissions.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        map.iter()
            .find(|(_, entry)| entry.conv_public_id == conv_public_id)
            .map(|(id, entry)| temps_ai::streaming::PermissionRequest {
                id: id.clone(),
                kind: entry.kind.clone(),
                tool_name: entry.tool_name.clone(),
                input: entry.input.clone(),
            })
    }

    /// Apply a permission-mode change to the currently running turn.
    ///
    /// Native provider tools and non-destructive Temps platform changes follow
    /// Auto mode. Questions and destructive platform changes remain explicit.
    /// The persisted conversation option is updated separately by the handler.
    pub fn apply_active_permission_mode(
        &self,
        conversation_id: i64,
        conv_public_id: &str,
        permission_mode: &str,
        auth: &AuthContext,
        metadata: &RequestMetadata,
    ) -> ActivePermissionModeUpdate {
        let auto = permission_mode_auto_approves_provider_tools(permission_mode);
        let active = {
            let turns = match self.active_turns.lock() {
                Ok(turns) => turns,
                Err(poisoned) => poisoned.into_inner(),
            };
            turns
                .get(&conversation_id)
                .map(|turn| turn.auto_approve_provider_tools.clone())
        };
        let Some(active) = active else {
            return ActivePermissionModeUpdate {
                applied_to_active_turn: false,
                auto_approved: Vec::new(),
            };
        };
        active.store(auto, Ordering::Release);
        if !auto {
            return ActivePermissionModeUpdate {
                applied_to_active_turn: true,
                auto_approved: Vec::new(),
            };
        }

        let pending = {
            let mut registry = match self.pending_permissions.lock() {
                Ok(registry) => registry,
                Err(poisoned) => poisoned.into_inner(),
            };
            let decisions = registry
                .iter()
                .filter_map(|(id, entry)| {
                    (entry.conv_public_id == conv_public_id)
                        .then(|| {
                            automatic_pending_decision(entry).map(|decision| (id.clone(), decision))
                        })
                        .flatten()
                })
                .collect::<Vec<_>>();
            decisions
                .into_iter()
                .filter_map(|(id, decision)| {
                    registry.remove(&id).map(|entry| (id, entry, decision))
                })
                .collect::<Vec<_>>()
        };
        let auto_approved = pending
            .into_iter()
            .map(|(id, entry, decision)| {
                let tool_name = entry.tool_name;
                let delivered = entry
                    .sender
                    .send(PermissionResolution {
                        decision,
                        auth: auth.clone(),
                        metadata: metadata.clone(),
                    })
                    .is_ok();
                AutoApprovedPermission {
                    id,
                    tool_name,
                    delivered,
                }
            })
            .collect();
        ActivePermissionModeUpdate {
            applied_to_active_turn: true,
            auto_approved,
        }
    }

    /// All turns of a conversation, oldest first.
    pub async fn messages(
        &self,
        conversation_id: i64,
    ) -> Result<Vec<ai_messages::Model>, ChatError> {
        Ok(ai_messages::Entity::find()
            .filter(ai_messages::Column::ConversationId.eq(conversation_id))
            .order_by_asc(ai_messages::Column::CreatedAt)
            .order_by_asc(ai_messages::Column::Id)
            .all(self.db.as_ref())
            .await?)
    }

    /// A bounded page of client-visible conversation messages.
    ///
    /// `before_message_id` is exclusive. With no cursor this loads the newest
    /// page. The database query runs newest-first so the limit can be applied
    /// efficiently, then the selected page is reversed to the oldest-first
    /// order expected by transcript renderers. Internal system and summary
    /// rows never leave the service.
    pub async fn messages_page(
        &self,
        conversation_id: i64,
        before_message_id: Option<i64>,
        limit: u64,
    ) -> Result<MessagePage, ChatError> {
        let mut query = ai_messages::Entity::find()
            .filter(ai_messages::Column::ConversationId.eq(conversation_id))
            .filter(ai_messages::Column::Role.ne("system"))
            .filter(ai_messages::Column::Role.ne("summary"));
        if let Some(before_message_id) = before_message_id {
            query = query.filter(ai_messages::Column::Id.lt(before_message_id));
        }

        let mut messages = query
            .order_by_desc(ai_messages::Column::Id)
            .limit(limit.saturating_add(1))
            .all(self.db.as_ref())
            .await?;

        // MockDatabase does not apply SQL predicates to supplied rows, and this
        // defense also keeps future alternate backends from accidentally
        // exposing internal context if their query implementation drifts.
        messages.retain(|message| !matches!(message.role.as_str(), "system" | "summary"));

        let page_size = usize::try_from(limit).unwrap_or(usize::MAX);
        let has_more = messages.len() > page_size;
        if has_more {
            messages.truncate(page_size);
        }
        messages.reverse();

        let next_before = has_more
            .then(|| {
                messages
                    .first()
                    .map(|message| encode_message_before_cursor(message.id))
            })
            .flatten();

        Ok(MessagePage {
            messages,
            has_more,
            next_before,
        })
    }

    /// Find or create the current user's conversation for a context. Context
    /// identity is per creator, so project members never share stored results
    /// or resumable CLI sessions.
    #[allow(clippy::too_many_arguments)]
    pub async fn get_or_create(
        &self,
        project_id: Option<i32>,
        context_type: &str,
        context_id: &str,
        user_id: i32,
        requested_provider: Option<&str>,
        requested_model: Option<&str>,
        requested_thinking_level: Option<&str>,
        requested_permission_mode: Option<&str>,
    ) -> Result<ai_conversations::Model, ChatError> {
        if let Some(existing) = self
            .find_by_context(project_id, user_id, context_type, context_id)
            .await?
        {
            return Ok(existing);
        }
        let provider = self
            .providers
            .get(context_type)
            .ok_or_else(|| ChatError::NoProvider(context_type.to_string()))?;
        if !provider.authorize(project_id, context_id).await {
            return Err(ChatError::ContextUnavailable);
        }
        let seed = provider
            .seed(project_id, context_id)
            .await
            .ok_or(ChatError::ContextUnavailable)?;

        let now = Utc::now();
        let runtime = self
            .resolve_conversation_runtime(
                requested_provider,
                requested_model,
                requested_thinking_level,
                requested_permission_mode,
            )
            .await?;
        let conv = ai_conversations::ActiveModel {
            public_id: Set(uuid::Uuid::new_v4().simple().to_string()),
            project_id: Set(project_id),
            application_id: Set(seed
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("application_id"))
                .and_then(serde_json::Value::as_i64)),
            context_type: Set(context_type.to_string()),
            context_id: Set(context_id.to_string()),
            title: Set(seed.title.clone()),
            status: Set("active".to_string()),
            created_by: Set(user_id),
            metadata: Set(seed.metadata.clone()),
            created_at: Set(now),
            last_activity_at: Set(now),
            ai_provider: Set(runtime.provider),
            ai_model: Set(runtime.model),
            ai_thinking_level: Set(runtime.thinking_level),
            ai_permission_mode: Set(runtime.permission_mode),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;

        self.insert_message(conv.id, "system", &seed.system, None)
            .await?;
        if let Some(first) = &seed.first_assistant {
            self.insert_message(conv.id, "assistant", first, None)
                .await?;
        }
        Ok(conv)
    }

    /// Resolve the exact runtime that `get_or_create` would use without
    /// mutating conversation or message storage. Existing conversations keep
    /// their pinned runtime; new conversations resolve instance defaults and
    /// validate provider capabilities. HTTP authorization uses this preflight
    /// before permitting `get_or_create` to perform its first insert.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn resolve_get_or_create_runtime(
        &self,
        project_id: Option<i32>,
        context_type: &str,
        context_id: &str,
        user_id: i32,
        requested_provider: Option<&str>,
        requested_model: Option<&str>,
        requested_thinking_level: Option<&str>,
        requested_permission_mode: Option<&str>,
    ) -> Result<ConversationRuntime, ChatError> {
        if let Some(existing) = self
            .find_by_context(project_id, user_id, context_type, context_id)
            .await?
        {
            return Ok(ConversationRuntime {
                provider: existing.ai_provider,
                model: existing.ai_model,
                thinking_level: existing.ai_thinking_level,
                permission_mode: existing.ai_permission_mode,
            });
        }
        self.resolve_conversation_runtime(
            requested_provider,
            requested_model,
            requested_thinking_level,
            requested_permission_mode,
        )
        .await
    }

    async fn resolve_conversation_runtime(
        &self,
        requested: Option<&str>,
        requested_model: Option<&str>,
        requested_thinking_level: Option<&str>,
        requested_permission_mode: Option<&str>,
    ) -> Result<ConversationRuntime, ChatError> {
        let requested_thinking_level = normalize_thinking_level(requested_thinking_level);
        let provider = match requested {
            Some(value) => value.to_string(),
            None => self.resolve_default_provider().await?,
        };
        let capabilities = self
            .ai
            .capabilities_for(Some(&provider), temps_ai::RefreshPolicy::Cached)
            .await
            .map_err(|error| {
                ChatError::Ai(format!(
                    "provider '{provider}' is not ready for a new conversation: {error}"
                ))
            })?;
        let model = requested_model
            .map(str::to_string)
            .or_else(|| capabilities.default_model_id.clone())
            .or_else(|| capabilities.models.first().map(|model| model.id.clone()))
            .unwrap_or_else(|| "default".to_string());
        let discovered_model = capabilities.model(&model);
        if discovered_model.is_none() && !(capabilities.models.is_empty() && model == "default") {
            return Err(ChatError::Ai(format!(
                "model '{model}' is not available for provider '{provider}'"
            )));
        }
        let thinking_level = match discovered_model {
            Some(discovered) => {
                // Project chat always attaches function tools. Providers may
                // advertise a narrower set of reasoning modes for tool turns.
                let valid_modes = discovered
                    .tool_thinking_modes
                    .as_ref()
                    .unwrap_or(&discovered.thinking_modes);
                let desired = requested_thinking_level
                    .map(str::to_string)
                    .or_else(|| discovered.default_thinking_mode_id.clone());
                match desired {
                    Some(value) if valid_modes.iter().any(|option| option.id == value) => {
                        Some(value)
                    }
                    Some(value)
                        if discovered.tool_thinking_modes.is_some()
                            && discovered
                                .thinking_modes
                                .iter()
                                .any(|option| option.id == value) =>
                    {
                        valid_modes.first().map(|option| option.id.clone())
                    }
                    Some(value) => {
                        return Err(ChatError::Ai(format!(
                            "thinking option '{value}' is not available for model '{model}'"
                        )));
                    }
                    None => valid_modes.first().map(|option| option.id.clone()),
                }
            }
            None if requested_thinking_level.is_some() => {
                return Err(ChatError::Ai(format!(
                    "thinking option '{}' is not available for model '{model}'",
                    requested_thinking_level.unwrap_or_default()
                )));
            }
            None => None,
        };
        let permission_mode = requested_permission_mode
            .map(str::to_string)
            .or(capabilities.default_permission_mode_id.clone())
            .ok_or_else(|| {
                ChatError::Ai(format!(
                    "provider '{provider}' has no default permission mode"
                ))
            })?;
        if capabilities.permission_mode(&permission_mode).is_none() {
            return Err(ChatError::Ai(format!(
                "permission mode '{permission_mode}' is not available for provider '{provider}'"
            )));
        }
        Ok(ConversationRuntime {
            provider,
            model,
            thinking_level,
            permission_mode,
        })
    }
    async fn resolve_default_provider(&self) -> Result<String, ChatError> {
        use temps_entities::ai_provider_keys;

        // Omitted providers belong to the API gateway only. A host harness is
        // an explicit, per-thread execution decision so it cannot become an
        // ambient capability for ordinary project chat.

        let key = ai_provider_keys::Entity::find()
            .filter(ai_provider_keys::Column::IsActive.eq(true))
            .order_by_asc(ai_provider_keys::Column::Id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ChatError::AiUnavailable)?;
        Ok(format!("gateway_key:{}", key.id))
    }

    async fn insert_message(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<ai_messages::Model, ChatError> {
        Ok(ai_messages::ActiveModel {
            conversation_id: Set(conversation_id),
            role: Set(role.to_string()),
            content: Set(content.to_string()),
            metadata: Set(metadata),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?)
    }

    /// Change turn-level runtime options while keeping the conversation's
    /// provider harness pinned. Models, reasoning, and permission modes are
    /// validated against that provider's current capabilities and persisted so
    /// reloads and subsequent turns use the same selection.
    pub async fn update_runtime_options(
        &self,
        conv: &ai_conversations::Model,
        model: Option<&str>,
        thinking_level: Option<&str>,
        permission_mode: Option<&str>,
    ) -> Result<ai_conversations::Model, ChatError> {
        let desired_model = model.unwrap_or(&conv.ai_model);
        let model_changed = desired_model != conv.ai_model;
        let current_thinking = normalize_thinking_level(conv.ai_thinking_level.as_deref());
        let desired_thinking = normalize_thinking_level(thinking_level)
            .or_else(|| (!model_changed).then_some(current_thinking).flatten());
        let desired_permission = permission_mode.unwrap_or(&conv.ai_permission_mode);
        if desired_model == conv.ai_model
            && desired_thinking == conv.ai_thinking_level.as_deref()
            && desired_permission == conv.ai_permission_mode
        {
            return Ok(conv.clone());
        }

        let runtime = self
            .resolve_conversation_runtime(
                Some(&conv.ai_provider),
                Some(desired_model),
                desired_thinking,
                Some(desired_permission),
            )
            .await?;
        if runtime.provider != conv.ai_provider {
            return Err(ChatError::Ai(
                "conversation provider cannot be changed after creation".to_string(),
            ));
        }
        ai_conversations::ActiveModel {
            id: Set(conv.id),
            // Provider sessions are model-specific. Starting a new harness
            // session makes the changed model deterministic; send_message then
            // replays the persisted history into that fresh session.
            cli_session_id: Set(cli_session_after_model_change(
                &conv.ai_model,
                &runtime.model,
                conv.cli_session_id.as_deref(),
            )),
            cli_session_fingerprint: Set(cli_session_fingerprint_after_model_change(
                &conv.ai_model,
                &runtime.model,
                conv.cli_session_fingerprint.as_deref(),
            )),
            ai_model: Set(runtime.model),
            ai_thinking_level: Set(runtime.thinking_level),
            ai_permission_mode: Set(runtime.permission_mode),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(ChatError::from)
    }

    /// Backwards-compatible helper used by callers that only change reasoning.
    pub async fn update_thinking_level(
        &self,
        conv: &ai_conversations::Model,
        thinking_level: &str,
    ) -> Result<ai_conversations::Model, ChatError> {
        self.update_runtime_options(conv, None, Some(thinking_level), None)
            .await
    }

    async fn touch(&self, conversation_id: i64) {
        let am = ai_conversations::ActiveModel {
            id: Set(conversation_id),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let _ = am.update(self.db.as_ref()).await;
    }

    /// Append a user message and start the assistant reply. Persists the user
    /// message up front and checkpoints one assistant draft throughout the turn,
    /// then finalizes that same row when generation completes (the `system` seed
    /// is already the first stored turn, so history replay is the full context).
    /// Errors before streaming starts return `Err`; later errors are published
    /// asynchronously over the conversation live wire.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_message(
        &self,
        conv: &ai_conversations::Model,
        turn_id: &str,
        user_text: &str,
        user_metadata: Option<serde_json::Value>,
        attachment_context: Option<&str>,
        // Optional client-supplied description of what the user is currently
        // viewing in the console (the page/entity). It is NOT persisted and NOT
        // shown in history — it's prepended to the user's message in-memory for
        // THIS turn only (see below), so the model can resolve "this trace" etc.
        page_context: Option<&str>,
        // The calling user's auth — forwarded to the tool loop so `call_api` can
        // replay GETs scoped to the user's own permissions.
        auth: &AuthContext,
        request_metadata: &RequestMetadata,
        // Canonical, live project-membership check. Chat ownership controls
        // transcript access, but it must never preserve access to private Git
        // source after the owner loses access to this project.
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    ) -> Result<(), ChatError> {
        let preparation_started = tokio::time::Instant::now();
        let mut phase_started = preparation_started;
        let mut log_phase = |phase: &'static str| {
            let now = tokio::time::Instant::now();
            tracing::info!(
            component = "ai_turn_timing",
                turn_id,
                conversation_id = conv.id,
                project_id = conv.project_id,
                provider = %conv.ai_provider,
                phase,
                phase_ms = now.duration_since(phase_started).as_millis() as u64,
                total_ms = now.duration_since(preparation_started).as_millis() as u64,
                "AI turn timing"
            );
            phase_started = now;
        };
        if !self.ai.is_available_for(Some(&conv.ai_provider)).await {
            return Err(ChatError::AiUnavailable);
        }
        log_phase("provider_readiness_checked");
        let mut application_seed_title = None;
        let mut sandbox_environment = temps_ai::SensitiveEnvironment::default();
        let harness_workspace = if conv.context_type == "application" {
            let workspaces = self.application_workspaces.as_ref().ok_or_else(|| {
                ChatError::Ai("application sandbox workspace is unavailable".to_string())
            })?;
            let application_public_id = conv
                .context_id
                .split(':')
                .next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ChatError::Ai("application thread has an invalid context id".to_string())
                })?;
            let applications = self.application_service.as_ref().ok_or_else(|| {
                ChatError::Ai("application topology service is unavailable".to_string())
            })?;
            let application = applications
                .get(auth.user_id(), application_public_id)
                .await?;
            application_seed_title = Some(application.application.name.clone());
            let mut workspace = workspaces
                .ensure(&application.application.public_id, &application.projects)
                .await?;
            let desired_workspace = applications.workspace(application.application.id).await?;
            if desired_workspace.desired_state == "quarantined" {
                return Err(ChatError::Ai(
                    "application workspace execution is quarantined because linked-project access could not be verified; restore access and explicitly resume the workspace"
                        .to_string(),
                ));
            }
            let sandboxes = self.application_sandboxes.as_ref().ok_or_else(|| {
                ChatError::Ai("application preview sandbox is unavailable".to_string())
            })?;
            let sandbox = sandboxes
                .get_or_create_application_workspace_with_config(
                    auth.user_id(),
                    &application.application.public_id,
                    application.primary_project_id,
                    workspace.host_work_dir.clone(),
                    (&desired_workspace).into(),
                )
                .await
                .map_err(|error| {
                    ChatError::Ai(format!(
                        "could not prepare the application execution sandbox: {error}"
                    ))
                })?;
            let project_ids = application
                .projects
                .iter()
                .map(|project| project.id)
                .collect::<Vec<_>>();
            sandbox_environment = temps_ai::SensitiveEnvironment::new(
                sandboxes
                    .runtime_environment(auth.user_id(), &sandbox.public_id)
                    .await
                    .map_err(|error| {
                        ChatError::Ai(format!(
                            "could not prepare the application sandbox's linked database variables: {error}"
                        ))
                    })?,
            );
            // Credential issuance reconciles the generic sandbox attachment
            // (the primary project). Finish with the full application
            // topology so databases linked through secondary projects remain
            // reachable for this shared workspace.
            sandboxes
                .synchronize_application_data_network(
                    auth.user_id(),
                    &application.application.public_id,
                    &sandbox.public_id,
                    &project_ids,
                )
                .await
                .map_err(|error| {
                    ChatError::Ai(format!(
                        "could not connect the application workspace to its linked databases: {error}"
                    ))
                })?;
            // Docker names first-class sandboxes with the opaque id suffix
            // (`temps-sandbox-<hex>`); keep `sbx_` in the database/API only.
            // The label is still unguessable and now recovers the exact
            // container that the preview gateway routes to.
            workspace.sandbox_label = sandbox
                .public_id
                .strip_prefix("sbx_")
                .unwrap_or(&sandbox.public_id)
                .to_string();
            Some(workspace)
        } else if conv.context_type == "global" {
            let applications = self.application_service.as_ref().ok_or_else(|| {
                ChatError::Ai("application topology service is unavailable".to_string())
            })?;
            let _workspace_quota_reservation = applications
                .reserve_global_workspace_quota(conv.created_by)
                .await?;
            let workspaces = self.application_workspaces.as_ref().ok_or_else(|| {
                ChatError::Ai("global sandbox workspace is unavailable".to_string())
            })?;
            // Global chats are distinct conversations but deliberately share
            // one user-scoped persistent workspace. A conversation UUID here
            // would create unbounded sandboxes and lose files between chats.
            let workspace_id = format!("global-user-{}", conv.created_by);
            let mut workspace = workspaces.ensure(&workspace_id, &[]).await?;
            let sandboxes = self.application_sandboxes.as_ref().ok_or_else(|| {
                ChatError::Ai("global operator sandbox is unavailable".to_string())
            })?;
            let sandbox = sandboxes
                .get_or_create_application_workspace(
                    auth.user_id(),
                    &workspace_id,
                    None,
                    workspace.host_work_dir.clone(),
                )
                .await
                .map_err(|error| {
                    ChatError::Ai(format!(
                        "could not prepare the global operator sandbox: {error}"
                    ))
                })?;
            workspace.sandbox_label = sandbox
                .public_id
                .strip_prefix("sbx_")
                .unwrap_or(&sandbox.public_id)
                .to_string();
            Some(workspace)
        } else {
            None
        };
        log_phase("application_workspace_ready");
        let user_message = self
            .insert_message(conv.id, "user", user_text, user_metadata)
            .await?;
        self.touch(conv.id).await;
        log_phase("user_message_persisted");

        // Cross-tab sync: a second tab watching this conversation never sees
        // the outgoing POST, so without this it has no way to learn a new
        // turn started or what the user typed. Best-effort (see
        // `publish_wire_event`) — never blocks or fails the turn.
        self.publish_wire_event(
            conv.id,
            "user_message",
            serde_json::json!({
                "turn_id": turn_id,
                "content": user_message.content,
                "created_at": user_message.created_at.to_rfc3339(),
                "attachments": user_message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("attachments")),
            })
            .to_string(),
        );

        let history = self.messages(conv.id).await?;
        log_phase("history_loaded");
        let is_first_user_turn = history.iter().filter(|m| m.role == "user").count() == 1;
        let should_capture_session_title = conv.context_type == "application"
            && conv.cli_session_id.is_none()
            && (is_first_user_turn || application_seed_title.as_deref() == conv.title.as_deref());

        // On the first user turn, generate an AI title from the message in the
        // background so the chat list shows a meaningful, content-derived label
        // instead of the generic seed title ("Project chat"). Fully decoupled
        // from the reply: a separate task that never blocks, holds open, or
        // fails the SSE stream, and runs at most once per conversation.
        if conv.context_type != "application" && is_first_user_turn {
            let ai = self.ai.clone();
            let db = self.db.clone();
            let conv_id = conv.id;
            let project_id = conv.project_id;
            let provider = conv.ai_provider.clone();
            let model = conv.ai_model.clone();
            let first_message = user_text.to_string();
            tokio::spawn(async move {
                generate_and_store_title(
                    &ai,
                    &db,
                    conv_id,
                    project_id,
                    provider,
                    model,
                    &first_message,
                )
                .await;
            });
        }
        let mut messages: Vec<ChatMessage> = history
            .iter()
            .filter(|m| matches!(m.role.as_str(), "system" | "user" | "assistant"))
            .map(|m| ChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
                ..Default::default()
            })
            .collect();

        // Refresh the system framing with the provider's CURRENT context (logs,
        // job failures, live status) on every turn, so the model always reasons
        // over up-to-date evidence — not the snapshot captured when the chat was
        // first created (which may predate the logs entirely). Best-effort: if
        // the provider can no longer build context, keep the stored system seed.
        if let Some(provider) = self.providers.get(conv.context_type.as_str()) {
            if let Some(seed) = provider.seed(conv.project_id, &conv.context_id).await {
                match messages.iter_mut().find(|m| m.role == "system") {
                    Some(sys) => sys.content = seed.system,
                    None => messages.insert(0, ChatMessage::system(seed.system)),
                }
            }
        }

        // Append the read-only API catalogue (the "API map") to the system
        // framing so the model can pick an operation_id by path directly, rather
        // than guessing keywords for search_api. Sourced from the API-tools
        // provider so it always reflects the live allowlist; merged into EVERY
        // context for the same reason its tools are.
        if let Some(api_tools_provider) = self.providers.get("__api_tools__") {
            if let Some(appendix) = api_tools_provider.system_appendix(auth) {
                match messages.iter_mut().find(|m| m.role == "system") {
                    Some(sys) => {
                        sys.content.push_str("\n\n");
                        sys.content.push_str(&appendix);
                    }
                    None => messages.insert(0, ChatMessage::system(appendix)),
                }
            }
        }

        // Ephemeral page context: the client tells us what the user is currently
        // viewing (e.g. a specific trace in a project). We prepend it to the
        // user's latest message in the IN-MEMORY turn only — it is never
        // persisted (history shows the raw message) and it rides at the tail (the
        // new user turn), so it adds nothing to the cacheable prompt prefix. This
        // lets the model resolve "this trace"/"this deployment" without the user
        // restating it.
        if let Some(pc) = page_context.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == "user") {
                last_user.content = format!(
                    "[Context — the user is currently viewing this page in the Temps console:\n{pc}\n]\n\n{}",
                    last_user.content
                );
            }
        }
        if let Some(attachments) = attachment_context
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if let Some(last_user) = messages
                .iter_mut()
                .rev()
                .find(|message| message.role == "user")
            {
                last_user.content = format!("{attachments}\n\n{}", last_user.content);
            }
        }

        // Gather the scoped tools available for this turn — the
        // context provider's own tools (e.g. a git-backed deployment can read
        // repo files) PLUS the shared, project-scoped trace tools (available in
        // every context when a trace store is configured) PLUS the ADR-024 generic
        // API meta-tools (search_api, describe_api, call_api) registered under the
        // sentinel context_type "__api_tools__". Gateway adapters receive
        // native function schemas; host adapters receive the same catalog via
        // their turn-scoped MCP bridge.
        let chat_capable = self.ai.chat_capable_for(Some(&conv.ai_provider)).await;
        let harness_execution = harness_workspace.is_some();
        let provider = self.providers.get(conv.context_type.as_str()).cloned();
        let mut tools: Vec<ChatTool> = Vec::new();
        if chat_capable {
            if let Some(p) = &provider {
                tools.extend(
                    p.tools_with_auth(conv.project_id, &conv.context_id, auth)
                        .await,
                );
            }
            // ADR-024: merge the generic API meta-tools from the sentinel provider.
            // This is done for EVERY conversation context so the model can always
            // search/describe/call the read-only REST API, regardless of context_type.
            if let Some(api_tools_provider) = self.providers.get("__api_tools__") {
                tools.extend(
                    api_tools_provider
                        .tools_with_auth(conv.project_id, &conv.context_id, auth)
                        .await,
                );
            }
            // Merge Git-repository exploration tools from the sentinel provider.
            // Gated only by the project having a Git connection (the provider
            // returns an empty vec when not connected). Available in every context
            // (project, alert, deployment, error-group, …) so the model can always
            // explore the source tree when a repo is connected, regardless of which
            // context_type seeded the chat.
            // ...but only for callers who hold the Git repository read
            // permission: the tools read private source with the project's
            // stored provider token, so `ProjectsRead` alone must not unlock
            // them (see `caller_may_use_repo_tools`).
            if project_repo_tools_allowed(auth, conv.project_id, project_access_checker.as_ref())
                .await
            {
                if let Some(repo_tools_provider) = self.providers.get("__repo_tools__") {
                    tools.extend(
                        repo_tools_provider
                            .tools_with_auth(conv.project_id, &conv.context_id, auth)
                            .await,
                    );
                }
            }

            // Write capability follows the native harness approval mode. The
            // operation still executes with the current user's RBAC and project
            // scope; there is no second project-level feature toggle.
            let write_appendix = self.maybe_add_write_tool(&mut tools, &messages, auth);
            if let Some(appendix) = write_appendix {
                // Append the write-CLI section map to the system framing so the model
                // knows what mutations are available and that they require confirmation.
                match messages.iter_mut().find(|m| m.role == "system") {
                    Some(sys) => {
                        sys.content.push_str("\n\n");
                        sys.content.push_str(&appendix);
                    }
                    None => messages.insert(0, ChatMessage::system(appendix)),
                }
            }
        }

        if harness_execution {
            // Filesystem/process access remains inside the persistent sandbox.
            // Platform access is separate: the harness receives only the
            // registered `temps`/`temps_write` MCP tools through a random
            // turn-scoped capability. It never receives the user's session, a
            // reusable API key, stored secret values, or arbitrary host access.
            if let Some(system) = messages.iter_mut().find(|message| message.role == "system") {
                system.content.push_str(
                    "\n\n## Development workspace\nYou are running inside a persistent Temps sandbox. The application projects are under ./projects. Use your native filesystem and terminal tools only inside this workspace. Use the registered `temps` MCP tool for platform reads and `temps_write` for confirm-gated platform changes such as creating services or deploying. The primary project's linked database variables are injected into your process and may be used by application code and child dev servers. Never print, inspect, persist, or commit their values. No reusable platform credential is present. Do not try to access host paths. Describe workspace changes and pending platform proposals clearly when you finish.",
                );
            }
        }
        log_phase("prompt_context_built");

        // Persist the assistant row before execution starts. Live WebSocket
        // events are only a notification channel; this draft is the
        // authoritative in-progress transcript used by reloads, reconnects,
        // and the approval-resume poll.
        let draft_message = self
            .insert_message(
                conv.id,
                "assistant",
                "",
                Some(serde_json::json!({
                    "draft": true,
                    "turn_id": turn_id,
                })),
            )
            .await?;

        // Every provider now enters the same turn runtime. The adapter decides
        // how to transport normalized text/tool/interaction events; chat owns
        // persistence, authorization, retries, and SSE for all providers.
        let detached_viewer = self
            .try_tool_loop_in_workspace(
                conv,
                messages,
                provider,
                tools,
                auth,
                request_metadata,
                project_access_checker,
                harness_workspace,
                sandbox_environment,
                Some(turn_id.to_string()),
                should_capture_session_title,
                Some(draft_message.id),
            )
            .await;
        log_phase("execution_task_spawned");
        // The WebSocket is the sole live event transport. Dropping this legacy
        // receiver detaches the unused in-process viewer without affecting the
        // server-owned task or its broadcast publication.
        drop(detached_viewer);
        Ok(())
    }

    /// If write support is wired, append the `temps_write` tool to `tools` and
    /// return the write-CLI root-help appendix for the system framing (so the model
    /// knows the confirm-gated mutation sections). Returns `None` when write support
    /// is absent or the handle is not yet populated.
    fn maybe_add_write_tool(
        &self,
        tools: &mut Vec<ChatTool>,
        _messages: &[ChatMessage],
        auth: &AuthContext,
    ) -> Option<String> {
        let ws = self.write_support.as_ref()?;
        let caller = ws.write_handle.get()?;
        // Full flat catalogue (not section-grouped) so the model sees every write
        // operation — a "redeploy" verb lives under `projects`, not `deployments`,
        // and section-guessing makes the model wrongly conclude an op is missing.
        let help = caller.cli_write_catalog(auth);
        tools.push(ChatTool {
            name: TEMPS_WRITE_TOOL_NAME.to_string(),
            description: "Request a mutation to the platform. \
                The tool follows the active harness permission mode (destructive changes always \
                require an inline approval), then returns \
                the real execution result or error to you in this same turn. React to that \
                result naturally: summarize success, respect rejection, and diagnose failure. \
                Use `--help` to discover write sections and operations exactly as with the \
                read-only `temps` tool, and ALWAYS read `<section> <operation> --help` to \
                confirm the operation does what the user actually asked BEFORE proposing it — \
                never pick an operation by its name alone (e.g. `promote_deployment` moves an \
                existing image to another environment; `rollback_to_deployment` reverts to an \
                older one; neither is a redeploy). If no available operation matches the \
                request, say so and ask — do NOT substitute a different operation. \
                Before proposing `create_service`, ALWAYS call the read-only `temps` tool with \
                `get_service_type_parameters --service_type <chosen-type>` in the same turn, \
                then copy its `x-temps-creation-defaults` name and parameter values unless the \
                user explicitly chose an override, and provide every field the schema marks as \
                required. Never replace the suggested unique name with the bare service type. \
                The generic \
                `parameters` object in `create_service --help` cannot express these \
                service-type-specific requirements. Do not guess them or learn them by \
                repeatedly submitting invalid proposals. \
                When the user wants the new database available to a project, pass that \
                project's real id as `--project_id` on `create_service`; this creates and \
                links it in one approval-gated operation. For a database that already \
                exists, use `link_service_to_project` after looking up both real ids. \
                Do not create an unlinked database unless the user explicitly asks for one. \
                To deploy files from an application workspace, use the exact \
                `deploy_application_workspace_project` operation with the application's \
                public id and a linked project id. It packages `projects/<slug>` and uses \
                the existing Drop pipeline; do not try to install a Temps token or run the \
                Temps CLI inside the sandbox. \
                Object and array flag values MUST be strict JSON with double-quoted keys and \
                string values, wrapped in single quotes so the CLI receives them intact (for \
                example `--parameters '{\"database\":\"postgres\",\"username\":\"postgres\"}'`). \
                Never emit JavaScript-style object literals such as `{database:postgres}`. \
                When an operation needs a concrete id or target you don't already have \
                (e.g. a redeploy via `trigger_project_pipeline` needs `--environment_id`, and \
                a container action needs a `container_id`), FIRST look it up with the read-only \
                `temps` tool (e.g. `environments get_environments`, or reuse an id already \
                returned by an earlier read such as `get_last_deployment`) and pass the real \
                value — do NOT omit a field the operation needs just because the schema marks \
                it optional, and never invent an id. \
                For a SEQUENCE of changes where order matters — e.g. raise an environment's \
                resources and THEN redeploy it so the new deploy picks them up — pass `commands` \
                (an ordered array), not repeated single calls: the user reviews the whole plan \
                and confirms each step in order, a step runs only after the previous one \
                succeeds, and a failed or rejected step halts the rest. Put prerequisites first, \
                and make sure every step's ids/flags are known up front (look them up first) — a \
                step cannot use a value produced by an earlier step. \
                Never claim success before this call returns status `executed`. When it returns \
                `failed`, use the supplied safe error to investigate and correct the request; \
                when it returns `rejected`, acknowledge the user's decision and do not restage \
                the same action unchanged."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "A single write Temps CLI command line (one action). \
                                        Discovery: `--help` → sections; `<section> --help` → operations; \
                                        `<section> <operation> --help` → flags. \
                                        Run: `<section> <operation> --flag value …`. \
                                        Object/array flags require strict JSON wrapped in single \
                                        quotes, e.g. `--parameters '{\"database\":\"postgres\"}'`. \
                                        In workspace or global chats, use the optional top-level \
                                        `project_id` selector for the target project. The server \
                                        re-checks the user's current access. \
                                        Omit it for global operations. \
                                        This pauses for inline approval and returns the actual outcome."
                    },
                    "commands": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "An ORDERED list of write CLI command lines to request as a \
                                        single multi-step plan (use instead of `command` when the \
                                        user asked for a sequence where order matters, e.g. \
                                        [\"update_environment_settings --env_id 8 --memory_limit 512\", \
                                        \"trigger_project_pipeline --environment_id 8\"]). Steps are \
                                        approved together, then run in this order; a step runs only \
                                        after the previous one succeeds. Provide exactly one of `command` or \
                                        `commands`."
                    },
                    "project_id": {
                        "type": "integer",
                        "description": "Optional project execution context for a workspace chat. Use only a project currently accessible to the user. Omit for global operations."
                    }
                },
                "additionalProperties": false
            }),
        });
        if !help.trim().is_empty() {
            Some(format!(
                "## The `temps_write` approval-gated mutation CLI\n\
                 You have a `temps_write` tool for platform mutations. Each call pauses on an \
                 inline approval and then returns the actual execution outcome in this turn. \
                 Do not ask the user to find another confirmation surface. Treat `executed` as \
                 success, `rejected` as the user's decision, and `failed` as evidence to diagnose \
                 before proposing a corrected action.\n\
                 Pick the operation that MATCHES the user's intent from the full list below \
                 (don't assume a verb lives in an obvious section — e.g. a redeploy/rebuild of \
                 a project is `trigger_project_pipeline`, not a `deployments` op). Read \
                 `<operation> --help` to verify flags, and never approximate with a \
                 similarly-named operation. Object/array flags must be strict JSON with \
                 double-quoted keys and string values, wrapped in single quotes. If nothing \
                 matches, say so and ask. Before `create_service`, always call the read-only \
                 `temps` operation `get_service_type_parameters --service_type <chosen-type>` \
                 in this turn, copy its `x-temps-creation-defaults` name and parameters unless \
                 the user supplied an override, and satisfy every required parameter from that \
                 returned schema. Never use the bare service type as the default name; \
                 the create operation's generic object schema cannot carry type-specific \
                 requirements. When a new database belongs to a project, include its real \
                 `--project_id` in `create_service` so creation and linking are one operation; \
                 use `link_service_to_project` for an existing database after looking up the \
                 service and project ids. Only leave a database unlinked when the user asks.\n\n\
                 For an application workspace deployment, use \
                 `deploy_application_workspace_project`; it packages the linked project's \
                 workspace directory and invokes Drop server-side without exposing a platform token.\n\n\
                 Available write operations (permissions permitting):\n```\n{help}```"
            ))
        } else {
            Some(
                "## The `temps_write` tool\nPlatform mutations use `temps_write`. The call follows \
                 the active harness permission mode and returns the real outcome to this turn. Only report \
                 success for status `executed`; react to `failed` or `rejected` using the returned \
                 safe details."
                    .to_string(),
            )
        }
    }

    /// Run the agentic tool loop and stream the result. Each round is a single
    /// streaming pass ([`AiService::chat_stream_turn`]) that yields assistant text
    /// **and** tool calls inline — so prose arrives token-by-token while tool
    /// activity surfaces live, from the same model call (the way the Vercel AI SDK
    /// works). When a round makes tool calls we execute them, feed the results
    /// back, and stream the next round; when a round answers in prose with no tool
    /// calls, that streamed prose is the final answer. A simple chat that needs no
    /// tools is therefore exactly one streaming call.
    ///
    /// The whole loop runs as a server-owned task. SSE and WebSocket clients are
    /// detachable viewers; closing or refreshing one never aborts execution.
    /// Completed turns persist `content` (all prose, for history replay) plus ordered
    /// `parts` (text/tool segments in occurrence order) and the executed `tools`,
    /// so a reload renders identically to the live stream.
    #[cfg(test)]
    async fn try_tool_loop(
        &self,
        conv: &ai_conversations::Model,
        base_messages: Vec<ChatMessage>,
        provider: Option<Arc<dyn ConversationContextProvider>>,
        tools: Vec<ChatTool>,
        auth: &AuthContext,
    ) -> Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>> {
        let request_metadata = RequestMetadata {
            ip_address: String::new(),
            user_agent: String::new(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: String::new(),
            scheme: String::new(),
            host: String::new(),
            is_secure: false,
        };
        self.try_tool_loop_in_workspace(
            conv,
            base_messages,
            provider,
            tools,
            auth,
            &request_metadata,
            None,
            None,
            temps_ai::SensitiveEnvironment::default(),
            None,
            false,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn try_tool_loop_in_workspace(
        &self,
        conv: &ai_conversations::Model,
        base_messages: Vec<ChatMessage>,
        provider: Option<Arc<dyn ConversationContextProvider>>,
        tools: Vec<ChatTool>,
        auth: &AuthContext,
        request_metadata: &RequestMetadata,
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
        harness_workspace: Option<temps_ai::HarnessWorkspace>,
        sandbox_environment: temps_ai::SensitiveEnvironment,
        active_turn_id: Option<String>,
        should_capture_session_title: bool,
        draft_message_id: Option<i64>,
    ) -> Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>> {
        // A turn is bounded by TIME, not by a number of steps.
        //
        // A step count is the wrong governor: it says nothing about cost or
        // about how long someone has been watching a spinner, and it cuts short
        // exactly the long, productive tasks people want the chat for. The user
        // can already see every tool call as it happens and press Stop through
        // the explicit server cancellation endpoint. What a
        // deadline adds is the guarantee those controls can't give: that an
        // unattended turn still ends, whatever the model is doing.
        //
        // The value is operator-tunable in Settings → AI, because the right
        // ceiling is a property of the model: a full alert-suggestion turn runs
        // ~10 minutes against a slow self-hosted model and seconds against a
        // hosted one. Read per turn (the settings service caches) so a change
        // applies to the next message rather than needing a restart.
        let max_turn_duration = match &self.config {
            Some(cfg) => match cfg.get_settings().await {
                Ok(settings) => settings.ai_chat_limits.turn_timeout(),
                Err(e) => {
                    // Never fail a turn because settings could not be read —
                    // fall back to the default ceiling and say why once.
                    tracing::warn!(
                        "Could not read AI chat limits ({e}); using the default turn timeout"
                    );
                    temps_core::AiChatLimitsSettings::default().turn_timeout()
                }
            },
            None => temps_core::AiChatLimitsSettings::default().turn_timeout(),
        };
        // Phrased without the number, since the limit is now configurable and a
        // hardcoded "15-minute" would start lying the moment anyone changes it.
        const TURN_TIMEOUT_REASON: &str =
            "reached the time limit for a single turn (configurable in Settings → AI)";
        // Purely an anti-spin guard, NOT a task budget. If rounds return
        // instantly (a provider erroring fast, a model emitting empty calls) the
        // deadline alone would allow an enormous number of iterations, so there
        // is a ceiling — set far beyond anything a real task approaches, so it
        // is never what ends a turn in practice.
        const MAX_ROUNDS: usize = 500;
        // Independent of both: a turn where every call was rejected several
        // rounds running is stuck, and stopping it in seconds beats letting it
        // grind for the rest of the deadline. This never shortens a turn that
        // is getting real results.
        const MAX_CONSECUTIVE_UNPRODUCTIVE_ROUNDS: usize = 3;
        // Every tool result is replayed on each later round, so without a cap on
        // the carried transcript a long turn runs out of context rather than
        // finishing. Not a limit on the task — a limit on what it re-sends.
        const MAX_CARRIED_TOOL_BYTES: usize = 192 * 1024;
        // Directive appended before the final, tool-free answer so the model
        // writes real prose from the evidence instead of narrating another tool
        // call it would like to make.
        const FINAL_DIRECTIVE: &str =
            "You have no more tool calls available. Using ONLY the tool results above, write \
             your final answer to my request now, in plain prose. Do not emit tool-call JSON \
             or describe tools you would call. If the data is insufficient, briefly state what \
             you found and what is still missing.";

        let harness_internal_api_url = if harness_workspace.is_some() {
            Some(match &self.config {
                Some(config) => config.resolve_internal_url().await,
                None => std::env::var("TEMPS_INTERNAL_API_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "http://host.docker.internal:8080".to_string()),
            })
        } else {
            None
        };

        // Own everything the server task needs (the service borrows `&self`).
        let ai = self.ai.clone();
        let db = self.db.clone();
        let api_tools = self.providers.get("__api_tools__").cloned();
        let repo_tools = self.providers.get("__repo_tools__").cloned();
        let conv_id = conv.id;
        let conv_public_id = conv.public_id.clone();
        let pending_permissions = self.pending_permissions.clone();
        let project_id = conv.project_id;
        let context_type = conv.context_type.clone();
        let context_id = conv.context_id.clone();
        let ai_provider = conv.ai_provider.clone();
        let ai_model = conv.ai_model.clone();
        let ai_thinking_level = conv.ai_thinking_level.clone();
        let ai_permission_mode = conv.ai_permission_mode.clone();
        let resume_session_id = harness_workspace.as_ref().and(conv.cli_session_id.clone());
        let auto_approve_provider_tools = Arc::new(AtomicBool::new(
            permission_mode_auto_approves_provider_tools(&ai_permission_mode),
        ));
        let task_auto_approve_provider_tools = auto_approve_provider_tools.clone();
        let initial_conversation_title = conv.title.clone();
        let auth = auth.clone();
        let request_metadata = request_metadata.clone();
        // Write support clones (None when not wired or project toggle is off).
        let write_handle_opt = self
            .write_support
            .as_ref()
            .and_then(|ws| ws.write_handle.get());
        let pending_svc_opt = self.write_support.as_ref().map(|ws| ws.pending.clone());
        let write_support_audit = self.write_support.as_ref().map(|ws| ws.audit.clone());

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();

        let turn_live = self.broadcast_sender_for(conv_id);
        let monitor_live = turn_live.clone();
        let monitor_db = self.db.clone();
        let active_turns = self.active_turns.clone();
        let harness_mcp_entries = self.harness_mcp_entries.clone();
        let monitor_turn_id = active_turn_id.clone();
        let task_turn_id = active_turn_id.clone();
        let monitor_started = tokio::time::Instant::now();
        let startup_event_tx = tx.clone();
        let startup_live = turn_live.clone();
        let (startup_sender, startup_receiver) = tokio::sync::oneshot::channel::<()>();

        // The loop, event publication, and persistence run independently of
        // the returned SSE receiver. Browser refresh only drops `rx`.
        let turn_task = tokio::spawn(async move {
            // `claim_turn` is durable before workspace/provider preparation is
            // complete. Stop can therefore arrive before this task is entered
            // in `active_turns`. Do not execute any harness or provider work
            // until the post-registration database check below proves this
            // exact turn still owns the server-side claim.
            if startup_receiver.await.is_err() {
                return true;
            }
            let mut messages = base_messages;
            let execution_state = Arc::new(tokio::sync::Mutex::new(ToolExecutionState::default()));
            let interaction_conv_public_id = conv_public_id.clone();
            let interaction_registry = pending_permissions.clone();
            type PermissionRegistrar = Arc<
                dyn Fn(
                        temps_ai::streaming::PermissionRequest,
                        PendingPermissionOrigin,
                    )
                        -> BoxFuture<'static, Result<PermissionResolution, temps_ai::AiError>>
                    + Send
                    + Sync,
            >;
            let interaction_registrar: PermissionRegistrar = Arc::new(move |request, origin| {
                let conv_public_id = interaction_conv_public_id.clone();
                let registry = interaction_registry.clone();
                let (sender, receiver) = tokio::sync::oneshot::channel();
                let generation = uuid::Uuid::new_v4();
                let safe_input = redact_value(&request.input);
                let entry = PendingPermissionEntry {
                    sender,
                    conv_public_id,
                    kind: request.kind.clone(),
                    tool_name: request.tool_name.clone(),
                    input: safe_input.clone(),
                    generation,
                    origin,
                };
                let inserted = match registry.lock() {
                    Ok(mut pending) => {
                        if pending.contains_key(&request.id) {
                            false
                        } else {
                            pending.insert(request.id.clone(), entry);
                            true
                        }
                    }
                    Err(poisoned) => {
                        let mut pending = poisoned.into_inner();
                        if pending.contains_key(&request.id) {
                            false
                        } else {
                            pending.insert(request.id.clone(), entry);
                            true
                        }
                    }
                };
                if !inserted {
                    return Box::pin(async move {
                        Err(temps_ai::AiError::Provider {
                            purpose: "chat.permission".to_string(),
                            reason: format!(
                                "permission request '{}' is already pending",
                                request.id
                            ),
                        })
                    });
                }

                let guard = PendingPermissionGuard {
                    registry: registry.clone(),
                    permission_id: request.id.clone(),
                    generation,
                };
                Box::pin(async move {
                    let result = receiver.await.map_err(|_| temps_ai::AiError::Provider {
                        purpose: "chat.permission".to_string(),
                        reason: format!(
                            "permission request '{}' ended before it was resolved",
                            request.id
                        ),
                    });
                    drop(guard);
                    result
                })
            });
            let provider_interaction_registrar = interaction_registrar.clone();
            let provider_auto_approve = task_auto_approve_provider_tools.clone();
            let interactions: temps_ai::InteractionExecutor = Arc::new(move |request| {
                if provider_auto_approve.load(Ordering::Acquire)
                    && request.kind == PermissionKind::ToolApproval
                {
                    return Box::pin(async { Ok(PermissionDecision::AllowTool) });
                }
                let request_kind = request.kind.clone();
                let pending =
                    provider_interaction_registrar(request, PendingPermissionOrigin::Provider);
                // The mode may have switched to Auto between the first policy
                // check and registration. Re-check after the waiter exists:
                // either the mode-change path already claimed and resolved it,
                // or dropping this exact-generation future removes it before
                // returning the automatic decision. This closes the gap where
                // a newly registered request could otherwise be stranded.
                if provider_auto_approve.load(Ordering::Acquire)
                    && request_kind == PermissionKind::ToolApproval
                {
                    drop(pending);
                    return Box::pin(async { Ok(PermissionDecision::AllowTool) });
                }
                Box::pin(async move { Ok(pending.await?.decision) })
            });
            let sandbox_interaction_tx = tx.clone();
            let sandbox_interaction_live = turn_live.clone();
            let sandbox_interaction_registry = interactions.clone();
            let sandbox_auto_approve = task_auto_approve_provider_tools.clone();
            let sandbox_interactions: temps_ai::InteractionExecutor = Arc::new(move |request| {
                if sandbox_auto_approve.load(Ordering::Acquire)
                    && request.kind == PermissionKind::ToolApproval
                {
                    return Box::pin(async { Ok(PermissionDecision::AllowTool) });
                }
                // Calling the registry closure synchronously installs the
                // waiter before the event becomes visible, preventing a fast
                // approval click from racing a missing permission id.
                let pending = sandbox_interaction_registry(request.clone());
                // `interactions` performs its own post-registration policy
                // check. Mirror it here so an approval that was automatically
                // consumed during that race is never rendered as a ghost card.
                if sandbox_auto_approve.load(Ordering::Acquire)
                    && request.kind == PermissionKind::ToolApproval
                {
                    return pending;
                }
                let safe_input = redact_value(&request.input);
                emit_turn_event(
                    &sandbox_interaction_tx,
                    &sandbox_interaction_live,
                    Ok(ChatStreamEvent::PermissionRequested {
                        id: request.id,
                        kind: request.kind,
                        tool_name: request.tool_name,
                        input: safe_input,
                    }),
                );
                pending
            });
            let platform_interaction_registrar = interaction_registrar.clone();
            let platform_interaction_tx = tx.clone();
            let platform_interaction_live = turn_live.clone();
            let platform_auto_approve = task_auto_approve_provider_tools.clone();
            let platform_auto_auth = auth.clone();
            let platform_auto_metadata = request_metadata.clone();
            let platform_interactions: PlatformInteractionExecutor = Arc::new(move |request| {
                if platform_auto_approve.load(Ordering::Acquire) {
                    if let Some(decision) = automatic_platform_decision(&request) {
                        let auth = platform_auto_auth.clone();
                        let metadata = platform_auto_metadata.clone();
                        return Box::pin(async move {
                            Ok(PermissionResolution {
                                decision,
                                auth,
                                metadata,
                            })
                        });
                    }
                }
                // Registration happens synchronously before the event is
                // published, so a fast click can never race a missing waiter.
                let pending = platform_interaction_registrar(
                    request.clone(),
                    PendingPermissionOrigin::PlatformWrite,
                );
                // Permission mode can change while the request is being
                // registered. The active-mode update may already have claimed
                // it; otherwise dropping this exact-generation waiter removes
                // it before the automatic resolution is returned.
                if platform_auto_approve.load(Ordering::Acquire) {
                    if let Some(decision) = automatic_platform_decision(&request) {
                        drop(pending);
                        let auth = platform_auto_auth.clone();
                        let metadata = platform_auto_metadata.clone();
                        return Box::pin(async move {
                            Ok(PermissionResolution {
                                decision,
                                auth,
                                metadata,
                            })
                        });
                    }
                }
                let safe_input = redact_value(&request.input);
                emit_turn_event(
                    &platform_interaction_tx,
                    &platform_interaction_live,
                    Ok(ChatStreamEvent::PermissionRequested {
                        id: request.id,
                        kind: request.kind,
                        tool_name: request.tool_name,
                        input: safe_input,
                    }),
                );
                pending
            });
            let executor_state = execution_state.clone();
            let principal_id = auth.user_id();
            let executor_provider = provider.clone();
            let executor_api_tools = api_tools.clone();
            let executor_repo_tools = repo_tools.clone();
            let executor_write_handle = write_handle_opt.clone();
            let executor_pending = pending_svc_opt.clone();
            let executor_audit = write_support_audit.clone();
            let executor_interactions = platform_interactions.clone();
            let executor_context_id = context_id.clone();
            let executor_auth = auth.clone();
            let executor_project_access_checker = project_access_checker.clone();
            let turn_tool_executor: ToolExecutor = Arc::new(move |call: ToolCall| {
                let state = executor_state.clone();
                let provider = executor_provider.clone();
                let api_tools = executor_api_tools.clone();
                let repo_tools = executor_repo_tools.clone();
                let write_handle = executor_write_handle.clone();
                let pending = executor_pending.clone();
                let audit = executor_audit.clone();
                let interactions = executor_interactions.clone();
                let context_id = executor_context_id.clone();
                let auth = executor_auth.clone();
                let project_access_checker = executor_project_access_checker.clone();
                Box::pin(async move {
                    let mut state = state.lock().await;
                    Ok(dispatch_conversation_tool(
                        &call,
                        project_id,
                        conv_id,
                        &context_id,
                        &auth,
                        project_access_checker.as_ref(),
                        provider.as_ref(),
                        api_tools.as_ref(),
                        repo_tools.as_ref(),
                        write_handle.as_deref(),
                        pending.as_deref(),
                        audit.as_deref(),
                        Some(&interactions),
                        &mut state,
                    )
                    .await)
                })
            });
            let (harness_mcp_server, _harness_mcp_guard) = match (
                harness_workspace.as_ref(),
                harness_internal_api_url.as_deref(),
            ) {
                (Some(_), Some(internal_api_url)) => {
                    let (server, guard) = ConversationService::register_harness_mcp(
                        harness_mcp_entries,
                        internal_api_url,
                        principal_id,
                        tools.clone(),
                        turn_tool_executor.clone(),
                        sandbox_interactions.clone(),
                        max_turn_duration + Duration::from_secs(30),
                    );
                    (Some(server), Some(guard))
                }
                _ => (None, None),
            };
            // Structured record of each executed tool, persisted on the assistant
            // message's metadata so the chat replays its tool work after a reload.
            let mut tools_meta: Vec<serde_json::Value> = Vec::new();
            // Ordered render segments (text / tool, in occurrence order). Persisted
            // so a reload shows the same interleaving the live stream did.
            let mut parts: Vec<serde_json::Value> = Vec::new();
            // The open text segment being accumulated (flushed into `parts` when a
            // tool call interrupts it or the turn ends).
            let mut cur_text = String::new();
            // All assistant prose across the turn — the persisted `content` and the
            // history replayed to the model on the next turn.
            let mut content = String::new();
            // Text deltas can be very small. Checkpoint immediately, then at
            // most four times per second; structural events always checkpoint.
            let mut last_draft_checkpoint: Option<Instant> = None;
            // Did a round answer in prose (no tool calls)? Then we have the final
            // answer and stop; otherwise we may need a salvage call.
            let mut answered = false;
            // The last provider error seen while trying to produce this turn. Kept
            // so that a turn which ends up with nothing to show can explain WHY
            // instead of just stopping — see the empty-turn check after salvage.
            let mut last_provider_error: Option<String> = None;
            let mut provider_session_id: Option<String> = None;
            let mut provider_session_title: Option<String> = None;
            let mut resume_session_id = resume_session_id;
            // Why generation stopped, when it was a bound rather than the model
            // finishing. Reported to the user — a turn that halts for a reason
            // nobody states looks identical to one that simply gave up.
            let mut stop_reason: Option<&'static str> = None;
            // Consecutive rounds in which every tool call was rejected.
            let mut unproductive_streak = 0usize;
            let turn_started = tokio::time::Instant::now();
            let trace_id = task_turn_id.as_deref().unwrap_or("untracked");
            tracing::info!(
            component = "ai_turn_timing",
                turn_id = trace_id,
                conversation_id = conv_id,
                project_id,
                provider = %ai_provider,
                phase = "execution_loop_started",
                total_ms = 0_u64,
                "AI turn timing"
            );

            'rounds: for round_index in 0..MAX_ROUNDS {
                if turn_started.elapsed() >= max_turn_duration {
                    stop_reason = Some(TURN_TIMEOUT_REASON);
                    break 'rounds;
                }
                let req = ChatTurnRequest {
                    trace_id: task_turn_id.clone(),
                    purpose: format!("chat.{context_type}.tools"),
                    project_id,
                    principal_id: Some(principal_id),
                    provider: Some(ai_provider.clone()),
                    model: Some(ai_model.clone()),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: Some(ai_permission_mode.clone()),
                    resume_session_id: resume_session_id.clone(),
                    messages: messages.clone(),
                    tools: tools.clone(),
                    harness_workspace: harness_workspace.clone(),
                    sandbox_environment: sandbox_environment.clone(),
                    harness_mcp_server: harness_mcp_server.clone(),
                    capture_session_title: should_capture_session_title,
                    ..Default::default()
                };
                // A single streaming pass: text deltas and tool calls arrive
                // inline. An error here (e.g. the model can't do tools) ends the
                // loop; the salvage below still tries a tool-free reply.
                let provider_call_started = tokio::time::Instant::now();
                let mut stream = match ai
                    .chat_stream_turn_with_services(
                        req,
                        temps_ai::TurnServices {
                            tools: Some(turn_tool_executor.clone()),
                            interactions: Some(interactions.clone()),
                        },
                    )
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::info!(
                        component = "ai_turn_timing",
                                        turn_id = trace_id,
                                        conversation_id = conv_id,
                                        project_id,
                                        provider = %ai_provider,
                                        phase = "provider_stream_failed",
                                        round = round_index + 1,
                                        phase_ms = provider_call_started.elapsed().as_millis() as u64,
                                        total_ms = turn_started.elapsed().as_millis() as u64,
                                        "AI turn timing"
                                    );
                        let reason = e.to_string();
                        tracing::warn!(
                            "chat_stream_turn failed for conv {conv_id} (round): {reason}"
                        );
                        if resume_session_id.is_some()
                            && provider_resume_session_is_missing(&ai_provider, &reason)
                        {
                            tracing::warn!(
                                conversation_id = conv_id,
                                provider = %ai_provider,
                                "provider resume session is missing; rebuilding it from durable conversation history"
                            );
                            resume_session_id = None;
                            if let Err(error) =
                                clear_missing_provider_session(db.as_ref(), conv_id).await
                            {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    %error,
                                    "failed to clear missing provider session id"
                                );
                            }
                            continue 'rounds;
                        }
                        last_provider_error = Some(reason);
                        break 'rounds;
                    }
                };
                tracing::info!(
                component = "ai_turn_timing",
                        turn_id = trace_id,
                        conversation_id = conv_id,
                        project_id,
                        provider = %ai_provider,
                        phase = "provider_stream_ready",
                        round = round_index + 1,
                        phase_ms = provider_call_started.elapsed().as_millis() as u64,
                        total_ms = turn_started.elapsed().as_millis() as u64,
                        "AI turn timing"
                    );
                let mut round_text = String::new();
                let mut round_calls: Vec<ToolCall> = Vec::new();
                let mut native_tool_ids = std::collections::HashSet::new();
                // Did anything this round return usable data (vs. only rejections)?
                let mut round_produced_something = false;
                let mut first_delta_seen = false;
                while let Some(item) = stream.next().await {
                    if !first_delta_seen {
                        first_delta_seen = true;
                        let delta_kind = match &item {
                            Ok(ChatStreamDelta::Text(_)) => "text",
                            Ok(ChatStreamDelta::ToolCall(_)) => "tool_call",
                            Ok(ChatStreamDelta::ToolResult { .. }) => "tool_result",
                            Ok(ChatStreamDelta::PermissionRequested(_)) => "permission",
                            Ok(ChatStreamDelta::SessionMetadata { .. }) => "session_metadata",
                            Err(_) => "error",
                        };
                        tracing::info!(
                        component = "ai_turn_timing",
                                        turn_id = trace_id,
                                        conversation_id = conv_id,
                                        project_id,
                                        provider = %ai_provider,
                                        phase = "provider_first_delta",
                                        round = round_index + 1,
                                        delta_kind,
                                        phase_ms = provider_call_started.elapsed().as_millis() as u64,
                                        total_ms = turn_started.elapsed().as_millis() as u64,
                                        "AI turn timing"
                                    );
                    }
                    match item {
                        Ok(ChatStreamDelta::Text(t)) => {
                            // Separate this round's prose from anything already shown
                            // (e.g. a previous round's narration) with a blank line.
                            if round_text.is_empty()
                                && !content.is_empty()
                                && !content.ends_with('\n')
                            {
                                let sep = "\n\n".to_string();
                                content.push_str(&sep);
                                cur_text.push_str(&sep);
                                emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(sep)));
                            }
                            round_text.push_str(&t);
                            content.push_str(&t);
                            cur_text.push_str(&t);
                            emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(t)));
                            if last_draft_checkpoint
                                .is_none_or(|last| last.elapsed() >= Duration::from_millis(250))
                            {
                                if let Some(message_id) = draft_message_id {
                                    if let Err(error) = persist_assistant_message(
                                        db.as_ref(),
                                        message_id,
                                        &content,
                                        &tools_meta,
                                        &parts,
                                        &cur_text,
                                        true,
                                    )
                                    .await
                                    {
                                        tracing::warn!(
                                            conv_id,
                                            message_id,
                                            %error,
                                            "failed to checkpoint assistant text"
                                        );
                                    }
                                    last_draft_checkpoint = Some(Instant::now());
                                }
                            }
                        }
                        Ok(ChatStreamDelta::ToolCall(tc)) => {
                            // Close any open text part so order is preserved, then
                            // surface the call live (the result follows once it runs).
                            if !cur_text.is_empty() {
                                parts.push(serde_json::json!({
                                    "type": "text",
                                    "text": std::mem::take(&mut cur_text),
                                }));
                            }
                            let display_arguments = redact_json_string(&tc.arguments);
                            record_tool_call(
                                &mut tools_meta,
                                &mut parts,
                                &tc.id,
                                &tc.name,
                                &display_arguments,
                            );
                            if let Some(message_id) = draft_message_id {
                                if let Err(error) = persist_assistant_message(
                                    db.as_ref(),
                                    message_id,
                                    &content,
                                    &tools_meta,
                                    &parts,
                                    &cur_text,
                                    true,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conv_id,
                                        message_id,
                                        %error,
                                        "failed to checkpoint assistant tool call"
                                    );
                                }
                                last_draft_checkpoint = Some(Instant::now());
                            }
                            emit_turn_event(
                                &tx,
                                &turn_live,
                                Ok(ChatStreamEvent::ToolCall {
                                    id: tc.id.clone(),
                                    name: tc.name.clone(),
                                    arguments: display_arguments,
                                }),
                            );
                            // A sandbox harness has already executed this
                            // native tool itself. In particular, a background
                            // Bash job may not emit a terminal tool_result
                            // until the process exits. Never hand that call to
                            // the outer API-tool executor while waiting: doing
                            // so replays unknown native tools for up to the
                            // 500-round anti-spin guard and produces the
                            // misleading "unusually long run" fallback.
                            if harness_workspace.is_some() {
                                native_tool_ids.insert(tc.id.clone());
                            }
                            round_calls.push(tc);
                        }
                        Ok(ChatStreamDelta::ToolResult { call, result }) => {
                            native_tool_ids.insert(call.id.clone());
                            let display_arguments = redact_json_string(&call.arguments);
                            let display_result = redact_json_string(&result);
                            emit_turn_event(
                                &tx,
                                &turn_live,
                                Ok(ChatStreamEvent::ToolResult {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                    content: display_result.clone(),
                                }),
                            );
                            record_tool_result(
                                &mut tools_meta,
                                &mut parts,
                                &call.id,
                                &call.name,
                                &display_arguments,
                                &display_result,
                            );
                            if let Some(message_id) = draft_message_id {
                                if let Err(error) = persist_assistant_message(
                                    db.as_ref(),
                                    message_id,
                                    &content,
                                    &tools_meta,
                                    &parts,
                                    &cur_text,
                                    true,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conv_id,
                                        message_id,
                                        %error,
                                        "failed to checkpoint assistant tool result"
                                    );
                                }
                                last_draft_checkpoint = Some(Instant::now());
                            }
                            round_produced_something = true;
                        }
                        Ok(ChatStreamDelta::PermissionRequested(perm)) => {
                            // The gateway AI path (OpenAI/Anthropic API) never emits
                            // this — it only comes from `run_interactive` on the
                            // interactive CLI path, which has its own streaming channel.
                            // If it somehow arrives here, forward it to the client
                            // (harmless) and log so operators can investigate.
                            tracing::warn!(
                                conv_id,
                                permission_id = %perm.id,
                                tool_name = %perm.tool_name,
                                "PermissionRequested delta arrived in the gateway tool loop \
                                 (unexpected — only expected on the interactive CLI path); \
                                 forwarding to client"
                            );
                            if let Some(message_id) = draft_message_id {
                                if let Err(error) = persist_assistant_message(
                                    db.as_ref(),
                                    message_id,
                                    &content,
                                    &tools_meta,
                                    &parts,
                                    &cur_text,
                                    true,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conv_id,
                                        message_id,
                                        %error,
                                        "failed to checkpoint assistant before permission request"
                                    );
                                }
                            }
                            emit_turn_event(
                                &tx,
                                &turn_live,
                                Ok(ChatStreamEvent::PermissionRequested {
                                    id: perm.id,
                                    kind: perm.kind,
                                    tool_name: perm.tool_name,
                                    input: perm.input,
                                }),
                            );
                        }
                        Ok(ChatStreamDelta::SessionMetadata { session_id, title }) => {
                            if session_id.is_some() {
                                provider_session_id = session_id;
                            }
                            if title.is_some() {
                                provider_session_title = title;
                            }
                        }
                        Err(e) => {
                            let reason = e.to_string();
                            tracing::warn!(
                                "chat_stream_turn item error for conv {conv_id}: {reason}"
                            );
                            if resume_session_id.is_some()
                                && round_text.is_empty()
                                && round_calls.is_empty()
                                && provider_resume_session_is_missing(&ai_provider, &reason)
                            {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    provider = %ai_provider,
                                    "provider resume session is missing; rebuilding it from durable conversation history"
                                );
                                resume_session_id = None;
                                if let Err(error) =
                                    clear_missing_provider_session(db.as_ref(), conv_id).await
                                {
                                    tracing::warn!(
                                        conversation_id = conv_id,
                                        %error,
                                        "failed to clear missing provider session id"
                                    );
                                }
                                continue 'rounds;
                            }
                            // Provider subprocess failures arrive as stream items
                            // after `chat_stream_turn_with_executor` has returned.
                            // Preserve the concrete reason so an empty turn reports
                            // the authentication/model error instead of the generic
                            // "provider returned no response" fallback.
                            last_provider_error = Some(reason);
                            break;
                        }
                    }
                }
                tracing::info!(
                component = "ai_turn_timing",
                        turn_id = trace_id,
                        conversation_id = conv_id,
                        project_id,
                        provider = %ai_provider,
                        phase = "provider_stream_complete",
                        round = round_index + 1,
                        phase_ms = provider_call_started.elapsed().as_millis() as u64,
                        total_ms = turn_started.elapsed().as_millis() as u64,
                        "AI turn timing"
                    );

                if !native_tool_ids.is_empty() {
                    round_calls.retain(|call| !native_tool_ids.contains(&call.id));
                    if round_calls.is_empty() {
                        answered = !round_text.is_empty();
                        break 'rounds;
                    }
                }

                if round_calls.is_empty() {
                    // The model answered in prose — that streamed text is the final
                    // answer. Done.
                    answered = true;
                    break 'rounds;
                }

                // Record the assistant's tool-call turn for the next round's context.
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: round_text,
                    tool_calls: Some(round_calls.clone()),
                    tool_call_id: None,
                });
                for tc in &round_calls {
                    // Route the ADR-024 `temps` CLI tool to the API-tools provider;
                    // `temps_write` to the write-proposal path;
                    // otherwise to the context provider. `project_id` is always the
                    // conversation's project, never anything the model supplied — so
                    // a tool can't be steered to another tenant's data.
                    let result = {
                        let mut state = execution_state.lock().await;
                        dispatch_conversation_tool(
                            tc,
                            project_id,
                            conv_id,
                            &context_id,
                            &auth,
                            project_access_checker.as_ref(),
                            provider.as_ref(),
                            api_tools.as_ref(),
                            repo_tools.as_ref(),
                            write_handle_opt.as_deref(),
                            pending_svc_opt.as_deref(),
                            write_support_audit.as_deref(),
                            Some(&platform_interactions),
                            &mut state,
                        )
                        .await
                    };
                    let display_arguments = redact_json_string(&tc.arguments);
                    let display_result = public_tool_result(&tc.name, &result);
                    // Surface the result right after — live.
                    emit_turn_event(
                        &tx,
                        &turn_live,
                        Ok(ChatStreamEvent::ToolResult {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: display_result.clone(),
                        }),
                    );
                    record_tool_result(
                        &mut tools_meta,
                        &mut parts,
                        &tc.id,
                        &tc.name,
                        &display_arguments,
                        &display_result,
                    );
                    if let Some(message_id) = draft_message_id {
                        if let Err(error) = persist_assistant_message(
                            db.as_ref(),
                            message_id,
                            &content,
                            &tools_meta,
                            &parts,
                            &cur_text,
                            true,
                        )
                        .await
                        {
                            tracing::warn!(
                                conv_id,
                                message_id,
                                %error,
                                "failed to checkpoint dispatched tool result"
                            );
                        }
                        last_draft_checkpoint = Some(Instant::now());
                    }
                    if tool_result_is_productive(&result) {
                        round_produced_something = true;
                    }
                    messages.push(ChatMessage::tool(tc.id.clone(), result));
                }

                // A round where every call was rejected is not progress. Allow a
                // couple — correcting a flag name is normal and the error
                // messages are written to be self-correcting — then stop, rather
                // than letting the model grind against the same mistake.
                if round_produced_something {
                    unproductive_streak = 0;
                } else {
                    unproductive_streak += 1;
                    if unproductive_streak >= MAX_CONSECUTIVE_UNPRODUCTIVE_ROUNDS {
                        stop_reason =
                            Some("stopped after several attempts in a row produced only errors");
                        break 'rounds;
                    }
                }

                // Bound what gets replayed next round. This, not the round
                // count, is what keeps a long turn from running out of context.
                trim_carried_tool_results(&mut messages, MAX_CARRIED_TOOL_BYTES);
            }

            // Fell out of the loop by exhausting the backstop rather than by
            // answering — worth saying, since it means the task was cut short.
            if !answered && stop_reason.is_none() && !tools_meta.is_empty() {
                stop_reason = Some("stopped after an unusually long run of steps");
            }

            // Close any trailing open text part.
            if !cur_text.is_empty() {
                parts.push(serde_json::json!({
                    "type": "text",
                    "text": std::mem::take(&mut cur_text),
                }));
            }

            // Say that the turn was cut short, before the salvage answer.
            //
            // Without this the user gets a confident-looking summary with no
            // indication it was written under duress — which reads as a complete
            // answer and is the same class of dishonesty as claiming a backtest
            // that never ran. They can then decide to ask it to continue.
            if let Some(reason) = stop_reason {
                let note = format!(
                    "\n\n_The assistant {reason}. What follows is based only on \
                     what it gathered so far — ask it to continue if that looks incomplete._\n\n"
                );
                content.push_str(&note);
                parts.push(serde_json::json!({ "type": "text", "text": note.clone() }));
                emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(note)));
            }

            // Salvage: the loop used tools but never settled on a prose answer (it
            // hit a bound while still calling tools). Make one tool-free streaming
            // call so the model answers from the evidence it gathered. The result
            // is persisted even when no browser is currently attached.
            if !answered && !tools_meta.is_empty() {
                let mut final_messages = messages;
                final_messages.push(ChatMessage::user(FINAL_DIRECTIVE));
                let req = ChatTurnRequest {
                    trace_id: task_turn_id.clone(),
                    purpose: format!("chat.{context_type}.tools.final"),
                    project_id,
                    principal_id: Some(principal_id),
                    provider: Some(ai_provider.clone()),
                    model: Some(ai_model.clone()),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: Some(ai_permission_mode.clone()),
                    resume_session_id: resume_session_id.clone(),
                    messages: final_messages,
                    harness_workspace: harness_workspace.clone(),
                    sandbox_environment: sandbox_environment.clone(),
                    ..Default::default()
                };
                let salvage = ai
                    .chat_stream_turn_with_services(
                        req,
                        temps_ai::TurnServices {
                            tools: None,
                            interactions: Some(interactions.clone()),
                        },
                    )
                    .await;
                if let Err(e) = &salvage {
                    tracing::warn!("chat_stream_turn failed for conv {conv_id} (salvage): {e}");
                    last_provider_error = Some(e.to_string());
                }
                if let Ok(mut stream) = salvage {
                    let mut salvage_text = String::new();
                    while let Some(item) = stream.next().await {
                        if let Ok(ChatStreamDelta::Text(t)) = item {
                            if salvage_text.is_empty()
                                && !content.is_empty()
                                && !content.ends_with('\n')
                            {
                                let sep = "\n\n".to_string();
                                content.push_str(&sep);
                                emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(sep)));
                            }
                            salvage_text.push_str(&t);
                            content.push_str(&t);
                            emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(t)));
                        }
                    }
                    if !salvage_text.is_empty() {
                        parts.push(serde_json::json!({ "type": "text", "text": salvage_text }));
                    }
                }
            }

            // The write contract is receipt-backed. A model can see an older
            // proposal in conversation history and incorrectly describe it as a
            // new one without calling the tool again. Never persist that claim as
            // success: there is no durable action for the UI to render or confirm.
            let proposal_not_staged =
                claims_proposal_was_staged(&content) && !has_fresh_proposal_receipt(&tools_meta);
            if proposal_not_staged {
                tracing::warn!(
                    conv_id,
                    "assistant claimed a proposal was staged without a current-turn receipt"
                );
                const CORRECTION: &str = "Temps could not verify this proposal because the AI did not submit it through the write tool. No approval card was created and no change was made. Retry to create a fresh proposal.";
                content = CORRECTION.to_string();
                // Keep attempted tool calls for diagnosis, but discard the
                // misleading prose and replace it with the server-owned truth.
                parts.retain(|part| {
                    part.get("type").and_then(serde_json::Value::as_str) == Some("tool")
                });
                parts.push(serde_json::json!({ "type": "text", "text": CORRECTION }));
                emit_turn_event(&tx, &turn_live, Err(ChatError::ProposalNotStaged));
            }

            // A turn that produced nothing at all — no prose, no tool work — is
            // indistinguishable from a hung request in the UI: the user's own
            // message sits there and nothing ever arrives. That is the worst
            // possible outcome for a self-hosted operator with no support channel
            // to ask, so say what went wrong. The client already renders an
            // `error` SSE event; before this it simply never received one, because
            // a provider failure just ended the loop.
            //
            let empty_turn_failed = content.is_empty() && tools_meta.is_empty();
            let turn_failed = proposal_not_staged || empty_turn_failed;
            if empty_turn_failed {
                // `ChatError::Ai` already renders an "AI provider error: " prefix,
                // so the message continues that sentence rather than restating it.
                let detail = last_provider_error
                    .unwrap_or_else(|| "the provider returned no response".to_string());
                emit_turn_event(
                    &tx,
                    &turn_live,
                    Err(ChatError::Ai(format!(
                        "no reply was produced. {detail} \
                         Review the concrete error and the selected harness/provider status, \
                         then try again."
                    ))),
                );
            }

            // Persist the assistant turn once complete. `content` is the full prose
            // for history replay; `metadata.tools` + `metadata.parts` let the UI
            // replay the tool work and interleaving on reload. Skip an entirely
            // empty turn.
            let response_bytes = content.len();
            let tool_count = tools_meta.len();
            if let Some(session_id) = provider_session_id {
                if let Err(error) = ai_conversations::Entity::update_many()
                    .filter(ai_conversations::Column::Id.eq(conv_id))
                    .col_expr(
                        ai_conversations::Column::CliSessionId,
                        Expr::value(Some(session_id)),
                    )
                    .exec(db.as_ref())
                    .await
                {
                    tracing::warn!(
                        conversation_id = conv_id,
                        %error,
                        "failed to persist harness session id"
                    );
                }
            }
            if should_capture_session_title && context_type == "application" {
                if let Some(title) = provider_session_title
                    .as_deref()
                    .map(clean_title)
                    .filter(|title| !title.is_empty())
                {
                    // Do not overwrite a manual rename made while the first
                    // turn was running: replace only the exact seed title the
                    // conversation had when this request started.
                    let condition = match initial_conversation_title.as_deref() {
                        Some(initial) => Condition::all()
                            .add(ai_conversations::Column::Id.eq(conv_id))
                            .add(ai_conversations::Column::Title.eq(initial.to_string())),
                        None => Condition::all()
                            .add(ai_conversations::Column::Id.eq(conv_id))
                            .add(ai_conversations::Column::Title.is_null()),
                    };
                    match ai_conversations::Entity::update_many()
                        .filter(condition)
                        .col_expr(
                            ai_conversations::Column::Title,
                            Expr::value(Some(title.clone())),
                        )
                        .exec(db.as_ref())
                        .await
                    {
                        Ok(result) if result.rows_affected == 1 => {
                            let _ = turn_live.send(WireEvent {
                                event: "conversation_title".to_string(),
                                data: serde_json::json!({ "title": title }).to_string(),
                            });
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(
                            conversation_id = conv_id,
                            %error,
                            "failed to store harness-provided conversation title"
                        ),
                    }
                }
            }
            if !content.is_empty() || !tools_meta.is_empty() {
                let persisted_message_id = match draft_message_id {
                    Some(message_id) => {
                        match persist_assistant_message(
                            db.as_ref(),
                            message_id,
                            &content,
                            &tools_meta,
                            &parts,
                            "",
                            false,
                        )
                        .await
                        {
                            Ok(()) => Some(message_id),
                            Err(error) => {
                                tracing::error!(
                                    conv_id,
                                    message_id,
                                    %error,
                                    "failed to finalize assistant draft"
                                );
                                None
                            }
                        }
                    }
                    None => {
                        let am = ai_messages::ActiveModel {
                            conversation_id: Set(conv_id),
                            role: Set("assistant".to_string()),
                            content: Set(content.clone()),
                            metadata: Set(assistant_message_metadata(&tools_meta, &parts, false)),
                            created_at: Set(Utc::now()),
                            ..Default::default()
                        };
                        am.insert(db.as_ref()).await.ok().map(|message| message.id)
                    }
                };
                if let Some(message_id) = persisted_message_id {
                    // Best-effort: link any pending actions created during this turn
                    // to the persisted assistant message so the UI can correlate them.
                    let proposed_action_ids =
                        execution_state.lock().await.proposed_action_ids.clone();
                    if !proposed_action_ids.is_empty() {
                        if let Some(pending) = &pending_svc_opt {
                            if let Err(e) =
                                pending.link_message(&proposed_action_ids, message_id).await
                            {
                                tracing::warn!(
                                    conv_id,
                                    "Failed to link pending actions to message {message_id}: {e}"
                                );
                            }
                        }
                    }
                }
            } else if let Some(message_id) = draft_message_id {
                // Preserve the previous behavior for a provider that produced
                // no assistant output at all: the structured failure event is
                // authoritative, not an empty assistant bubble.
                if let Err(error) = ai_messages::Entity::delete_by_id(message_id)
                    .exec(db.as_ref())
                    .await
                {
                    tracing::warn!(
                        conv_id,
                        message_id,
                        %error,
                        "failed to remove empty assistant draft"
                    );
                }
            }
            tracing::info!(
            component = "ai_turn_timing",
                turn_id = trace_id,
                conversation_id = conv_id,
                project_id,
                provider = %ai_provider,
                phase = "execution_task_complete",
                total_ms = turn_started.elapsed().as_millis() as u64,
                response_bytes,
                tool_count,
                failed = turn_failed,
                "AI turn timing"
            );
            turn_failed
        });

        if let Some(turn_id) = active_turn_id.as_ref() {
            let active = ActiveTurn {
                turn_id: turn_id.clone(),
                abort: turn_task.abort_handle(),
                auto_approve_provider_tools: auto_approve_provider_tools.clone(),
            };
            match active_turns.lock() {
                Ok(mut turns) => {
                    turns.insert(conv_id, active);
                }
                Err(poisoned) => {
                    poisoned.into_inner().insert(conv_id, active);
                }
            }
        }

        // The database is the source of truth for turn ownership. A Stop that
        // raced workspace preparation clears this claim even when there was no
        // in-memory abort handle yet; in that case dropping the barrier keeps
        // the newly spawned task from starting after it was cancelled.
        let startup_authorized = match active_turn_id.as_deref() {
            Some(turn_id) => match ai_conversations::Entity::find_by_id(conv_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(current)) => {
                    // A permission change may race the workspace preparation
                    // that precedes active-turn registration. Re-read the
                    // durable value before releasing the startup barrier so
                    // that change still applies to this turn.
                    auto_approve_provider_tools.store(
                        permission_mode_auto_approves_provider_tools(&current.ai_permission_mode),
                        Ordering::Release,
                    );
                    current.turn_status == "running"
                        && current.active_turn_id.as_deref() == Some(turn_id)
                }
                Ok(None) => false,
                Err(error) => {
                    tracing::error!(
                        conv_id,
                        turn_id,
                        error = %error,
                        "failed to verify AI turn ownership before execution"
                    );
                    emit_turn_event(
                        &startup_event_tx,
                        &startup_live,
                        Err(ChatError::Ai(
                            "The server could not verify this turn before execution. Please try again."
                                .to_string(),
                        )),
                    );
                    false
                }
            },
            // Unit-level tool-loop callers do not claim a durable turn.
            None => true,
        };
        if startup_authorized {
            let _ = startup_sender.send(());
        }

        // A monitor, not the SSE response, owns the task handle. It publishes a
        // terminal event and releases the persisted claim even when no browser
        // is currently attached.
        tokio::spawn(async move {
            let outcome = turn_task.await;
            if let Some(turn_id) = monitor_turn_id {
                match active_turns.lock() {
                    Ok(mut turns) => {
                        if turns
                            .get(&conv_id)
                            .is_some_and(|active| active.turn_id == turn_id)
                        {
                            turns.remove(&conv_id);
                        }
                    }
                    Err(poisoned) => {
                        let mut turns = poisoned.into_inner();
                        if turns
                            .get(&conv_id)
                            .is_some_and(|active| active.turn_id == turn_id)
                        {
                            turns.remove(&conv_id);
                        }
                    }
                }
                let terminal_status = match &outcome {
                    Ok(false) => "completed",
                    Ok(true) | Err(_) => "failed",
                };
                if let Err(error) = ai_conversations::Entity::update_many()
                    .filter(ai_conversations::Column::Id.eq(conv_id))
                    .filter(ai_conversations::Column::ActiveTurnId.eq(&turn_id))
                    .col_expr(
                        ai_conversations::Column::TurnStatus,
                        Expr::value(terminal_status),
                    )
                    .col_expr(
                        ai_conversations::Column::ActiveTurnId,
                        Expr::value(Option::<String>::None),
                    )
                    .col_expr(
                        ai_conversations::Column::TurnStartedAt,
                        Expr::value(Option::<chrono::DateTime<Utc>>::None),
                    )
                    .exec(monitor_db.as_ref())
                    .await
                {
                    tracing::error!(conv_id, turn_id, "failed to finalize AI turn: {error}");
                }
                tracing::info!(
                    component = "ai_turn_timing",
                    turn_id,
                    conversation_id = conv_id,
                    phase = "turn_finalized",
                    terminal_status,
                    total_ms = monitor_started.elapsed().as_millis() as u64,
                    "AI turn timing"
                );
                if outcome.as_ref().is_err_and(|error| error.is_panic()) {
                    let _ = monitor_live.send(WireEvent {
                        event: "error".to_string(),
                        data: "The server-side AI turn stopped unexpectedly.".to_string(),
                    });
                }
            }
            let _ = monitor_live.send(WireEvent {
                event: "turn_complete".to_string(),
                data: String::new(),
            });
        });

        let out = async_stream::stream! {
            while let Some(item) = rx.recv().await {
                yield item;
            }
        };
        Box::pin(out)
    }

    /// Archive a conversation (soft delete).
    pub async fn archive(&self, conv: &ai_conversations::Model) -> Result<(), ChatError> {
        if conv.turn_status == "running" {
            return Err(ChatError::TurnInProgress {
                conversation_id: conv.public_id.clone(),
            });
        }
        let am = ai_conversations::ActiveModel {
            id: Set(conv.id),
            status: Set("archived".to_string()),
            ..Default::default()
        };
        am.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Restore an archived conversation to the active thread list.
    pub async fn restore(&self, conv: &ai_conversations::Model) -> Result<(), ChatError> {
        let am = ai_conversations::ActiveModel {
            id: Set(conv.id),
            status: Set("active".to_string()),
            ..Default::default()
        };
        am.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Rename a conversation (set its human-facing title). Returns the updated
    /// model so the handler can echo the new title back to the client.
    pub async fn rename(
        &self,
        conv: &ai_conversations::Model,
        title: &str,
    ) -> Result<ai_conversations::Model, ChatError> {
        let am = ai_conversations::ActiveModel {
            id: Set(conv.id),
            title: Set(Some(title.to_string())),
            ..Default::default()
        };
        let updated = am.update(self.db.as_ref()).await?;
        Ok(updated)
    }
}

// ---------------------------------------------------------------------------
// Write-tool dispatch helper (free function so the spawned task can borrow it)
// ---------------------------------------------------------------------------

/// Did this tool result come from actually EXECUTING `operation_id` successfully?
///
/// Judged from the result, never from the arguments the model sent. Matching on
/// the request is how a safety control gets satisfied without doing anything:
/// `alerts preview_alert --help` mentions the operation, returns cheerful help
/// text, and would otherwise count as a backtest — the model could then propose
/// a threshold it never checked, and the human confirming would see it
/// described as verified.
///
/// The read CLI's execution path returns `{"operation":…,"status":…,"data":…}`,
/// so the operation that ran and the status it returned are both stated by the
/// executor. Help and every error return plain prose, which parses as nothing
/// and is correctly rejected.
fn executed_operation_ok(result: &str, operation_id: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(result) else {
        return false;
    };
    if v.get("operation").and_then(serde_json::Value::as_str) != Some(operation_id) {
        return false;
    }
    v.get("status")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|s| (200..300).contains(&s))
}

/// Cap what is kept for repeat-detection and the backtest check.
///
/// These two uses need to know *that* a call happened and roughly what came
/// back, not the whole payload — and a tool result can be 128 KiB. Retaining
/// every one at full size for the length of a turn would hold megabytes per
/// concurrent chat, and would quietly undo [`trim_carried_tool_results`], since
/// the repeat guard feeds this copy back into the transcript.
fn summarize_for_recall(result: &str) -> String {
    const MAX: usize = 4 * 1024;
    if result.len() <= MAX {
        return result.to_string();
    }
    // Cut on a char boundary — `result` is arbitrary tool output, so slicing
    // blind would panic on any multi-byte character straddling the limit.
    let mut end = MAX;
    while end > 0 && !result.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[… truncated for recall; re-run the command if you need the rest]",
        &result[..end]
    )
}

/// Did a tool call produce something the model can reason from, or only a
/// rejection?
///
/// This is the difference between a turn that is making progress and one that
/// is stuck, and the two need opposite treatment: a long productive task should
/// be allowed to continue, a loop of validation errors should be stopped
/// quickly. A single round counter cannot tell them apart, so it ends up too
/// tight for the first and far too loose for the second.
///
/// The read CLI has a defined success shape — `{"operation":…,"status":N,…}` —
/// so classifying it is parsing a contract, not guessing. Anything else is
/// assumed productive: a tool this function does not understand must not be
/// mistaken for a failure and used to cut a working turn short.
fn tool_result_is_productive(result: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(result) {
        Ok(v) => match v.get("status").and_then(serde_json::Value::as_u64) {
            Some(status) => (200..300).contains(&status),
            // Valid JSON without a status: the write tool's proposal receipt,
            // which is very much progress.
            None => true,
        },
        // Not JSON — every failure path returns a plain sentence, but so could a
        // future tool, hence the explicit allow-list of known rejections rather
        // than treating all prose as failure.
        Err(_) => {
            // Kept next to the messages they mirror. If a rejection is reworded
            // without updating this, the effect is that a stuck turn runs longer
            // (bounded by MAX_ROUNDS and the deadline) rather than a working turn
            // being cut short — the safe direction, but still worth keeping true.
            const REJECTIONS: &[&str] = &[
                "Unknown parameter(s)",
                "Missing required parameters",
                "has the wrong shape",
                "Required parameter",
                "Unknown operation",
                "Unknown command",
                "is not available",
                "You already ran",
                "Not staged",
                "Invalid `temps_write` arguments",
            ];
            !REJECTIONS.iter().any(|r| result.contains(r)) && !result.contains("` failed:")
        }
    }
}

/// Replace the oldest tool results with a stub once the carried-forward
/// transcript grows past `budget` bytes.
///
/// Every tool result is replayed to the model on every subsequent round, so a
/// long task re-sends everything it has ever read — the transcript, not the
/// round count, is what actually bounds how long a turn can run. Without this,
/// raising the round cap just moves the failure from "stopped early" to "blew
/// the context window", which is worse because it is not explained.
///
/// The messages are kept in place with their `tool_call_id` intact — providers
/// reject a tool_calls block whose answers have gone missing — and only the
/// content is replaced, so the model still sees that the call happened and what
/// it was.
fn trim_carried_tool_results(messages: &mut [ChatMessage], budget: usize) {
    let total: usize = messages
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.content.len())
        .sum();
    if total <= budget {
        return;
    }

    // Drop oldest-first: recent results are what the model is reasoning about
    // right now, and the older ones have usually been summarised into its own
    // prose already.
    let mut over = total - budget;
    for m in messages.iter_mut().filter(|m| m.role == "tool") {
        if over == 0 {
            break;
        }
        const STUB: &str = "[earlier tool result dropped to stay within the context budget —                             re-run the command if you still need it]";
        if m.content.len() <= STUB.len() {
            continue;
        }
        over = over.saturating_sub(m.content.len() - STUB.len());
        m.content = STUB.to_string();
    }
}

/// Operations that must not be proposed without first backtesting them, mapped
/// to the read-tool operation that does the backtesting.
///
/// A threshold is a number someone invented; the only thing that turns it into
/// a judgement is knowing how often it would have fired. The system prompt asks
/// for that and a model will still skip it — and worse, claim it did — so this
/// is enforced rather than requested. `preview_alert` is read-only and saves
/// nothing, so requiring it costs one extra round and no risk.
const BACKTEST_REQUIRED: &[(&str, &str)] = &[
    ("create_alert", "preview_alert"),
    ("update_alert", "preview_alert"),
];

/// If `command` proposes an operation that requires a backtest, and no matching
/// backtest was run this turn, return the message to send back instead.
///
/// Matching is by operation name only, not by exact arguments: the model
/// legitimately tunes a threshold between backtest and proposal, and demanding
/// byte-identical config would just make the requirement impossible to satisfy.
/// The goal is that the model has *looked*, not that it never adjusted after.
///
/// The check is deliberately scoped to the CURRENT turn. A follow-up like "now
/// create that alert" therefore backtests again, which reads like friction but
/// is the correct behaviour: it is a new proposal, the metric has moved since,
/// and a backtest from an earlier turn says nothing about the threshold being
/// proposed now. Honouring an older one would keep the requirement satisfiable
/// while quietly voiding what it guarantees — and the re-run is read-only and
/// costs a single round.
fn missing_backtest(
    command: &str,
    seen_calls: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // Match the operation in COMMAND POSITION only. The CLI form is
    // `[section] <operation> --flags…`, so scanning every token would also fire
    // on an operation name that merely appears inside an argument (a rule named
    // "create_alert", say) and demand a backtest for an unrelated call.
    let position = command
        .split_whitespace()
        .take_while(|t| !t.starts_with("--"))
        .take(2);
    let (op, backtest_op) = position
        .filter_map(|token| BACKTEST_REQUIRED.iter().find(|(op, _)| token == *op))
        .next()?;

    let ran_backtest = seen_calls
        .values()
        .any(|result| executed_operation_ok(result, backtest_op));
    if ran_backtest {
        return None;
    }

    // Spell the command out rather than describing it. The model reliably
    // rediscovers these flags by trial and error otherwise, and every wrong
    // guess costs a round out of the loop's budget — enough of them and the
    // turn ends with nothing proposed, which is a worse outcome than the
    // un-backtested proposal this gate exists to prevent.
    Some(format!(
        "Not staged: `{op}` requires a backtest first, and none succeeded this turn.\n\n\
         Run this with the `temps` tool, substituting the values you intend to propose:\n\n\
         alerts {backtest_op} --metric_name <metric> --aggregation <avg|max|p95|…> \
         --window_secs <secs> \
         --detection_config '{{\"kind\":\"static\",\"comparator\":\"gt\",\"threshold\":<number>}}'\n\n\
         It is read-only and saves nothing. It returns `breach_count` out of the points it \
         scored — how often this exact rule WOULD have fired. Judge the threshold by it (firing \
         on nearly every bucket is noise; never firing may be pointless), then propose the \
         configuration you settled on.\n\n\
         Do not describe a rule as backtested unless you have run this and read the result."
    ))
}

#[derive(Default)]
struct ToolExecutionState {
    proposed_action_ids: Vec<i64>,
    seen_calls: std::collections::HashMap<String, String>,
}

/// Whether this caller may drive the Git repo-exploration tools
/// (`read_repo_file`, `list_repo_dir`, `list_repo_branches`, `list_repo_tags`).
///
/// These tools read the project's private source through the stored Git
/// provider connection token, and their raw output is streamed straight back
/// to the caller in a `tool_result` SSE frame before anything could filter it.
/// The chat endpoints themselves only require `ProjectsRead`/`ProjectsWrite`,
/// so without this check a caller with no Git permission at all could ask the
/// model to read `.env`, credentials or any source file and get the bytes
/// verbatim — the repository permission would be enforced on
/// `/git/repositories/*` and nowhere else.
fn caller_may_use_repo_tools(auth: &AuthContext) -> bool {
    auth.has_permission(&temps_auth::permissions::Permission::GitRepositoriesRead)
}

/// Re-check live project membership in addition to the instance-level Git
/// permission. Conversation ownership intentionally survives membership
/// changes so the transcript remains available, but private source access must
/// not. Checker failures deny access rather than exposing repository data.
async fn project_repo_tools_allowed(
    auth: &AuthContext,
    project_id: Option<i32>,
    checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
) -> bool {
    if !caller_may_use_repo_tools(auth) {
        return false;
    }
    let Some(project_id) = project_id else {
        return false;
    };
    if auth.is_admin() || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
        return true;
    }
    let Some(checker) = checker else {
        // OSS has no team membership layer; the instance permission remains
        // the canonical authorization decision in that configuration.
        return true;
    };
    let Some(user_id) = auth.user_id_opt() else {
        return false;
    };
    match checker
        .effective_project_permissions(user_id, project_id)
        .await
    {
        Ok(Some(permissions)) => permissions.iter().any(|permission| {
            permission == &temps_auth::permissions::Permission::GitRepositoriesRead.to_string()
        }),
        Ok(None) => match checker.user_can_access_project(user_id, project_id).await {
            Ok(allowed) => allowed,
            Err(error) => {
                tracing::error!(
                    user_id,
                    project_id,
                    error = %error,
                    "failed to verify live project membership for AI repository tool"
                );
                false
            }
        },
        Err(error) => {
            tracing::error!(
                user_id,
                project_id,
                error = %error,
                "failed to verify live project repository permission for AI repository tool"
            );
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_conversation_tool(
    call: &ToolCall,
    project_id: Option<i32>,
    conversation_id: i64,
    context_id: &str,
    auth: &AuthContext,
    project_access_checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
    context_provider: Option<&Arc<dyn ConversationContextProvider>>,
    api_tools: Option<&Arc<dyn ConversationContextProvider>>,
    repo_tools: Option<&Arc<dyn ConversationContextProvider>>,
    write_handle: Option<&temps_ai_api_tools::InternalApiCaller>,
    pending: Option<&PendingActionService>,
    audit: Option<&dyn AuditLogger>,
    interactions: Option<&PlatformInteractionExecutor>,
    state: &mut ToolExecutionState,
) -> String {
    let call_key = format!("{}|{}", call.name, call.arguments.trim());
    if let Some(previous) = state.seen_calls.get(&call_key) {
        return format!(
            "You already ran `{}` with these exact arguments earlier this turn. \
             Do NOT repeat it. Use a different next step or answer from the previous result:\n\n{}",
            call.name, previous
        );
    }
    let result = if call.name == TEMPS_WRITE_TOOL_NAME {
        dispatch_write_tool(
            &call.arguments,
            project_id,
            conversation_id,
            context_id,
            auth,
            write_handle,
            pending,
            audit,
            interactions,
            &mut state.proposed_action_ids,
            &state.seen_calls,
        )
        .await
    } else if call.name == "temps" {
        if let Some(provider) = api_tools {
            provider
                .execute_tool_with_auth(project_id, context_id, &call.name, &call.arguments, auth)
                .await
        } else {
            "Tool 'temps' is not available (API tools provider absent).".to_string()
        }
    } else if matches!(
        call.name.as_str(),
        "read_repo_file" | "list_repo_dir" | "list_repo_branches" | "list_repo_tags"
    ) {
        if !project_repo_tools_allowed(auth, project_id, project_access_checker).await {
            // Defence in depth: the tool was never offered to the model for
            // this caller, but a model can still emit the call name from
            // memory, and dispatch must not honour it.
            format!(
                "Tool '{}' is not available: it requires current project access and the {} permission.",
                call.name,
                temps_auth::permissions::Permission::GitRepositoriesRead
            )
        } else if let Some(provider) = repo_tools {
            provider
                .execute_tool_with_auth(project_id, context_id, &call.name, &call.arguments, auth)
                .await
        } else {
            format!(
                "Tool '{}' is not available (repo tools provider absent).",
                call.name
            )
        }
    } else if let Some(provider) = context_provider {
        provider
            .execute_tool(project_id, context_id, &call.name, &call.arguments)
            .await
    } else {
        format!("Tool '{}' is not available in this context.", call.name)
    };
    state
        .seen_calls
        .insert(call_key, summarize_for_recall(&result));
    result
}

/// Dispatch a `temps_write` tool call: parse and validate the command, request
/// inline approval, execute with the approving user's current authorization,
/// and return the real execution result to the model in the same turn.
///
/// Returns a readable string result that goes back to the model as the tool
/// result — always, even on internal errors (never panics).
#[allow(clippy::too_many_arguments)]
async fn dispatch_write_tool(
    arguments: &str,
    project_id: Option<i32>,
    conversation_id: i64,
    context_id: &str,
    auth: &AuthContext,
    write_handle: Option<&temps_ai_api_tools::InternalApiCaller>,
    pending_svc: Option<&PendingActionService>,
    audit: Option<&dyn AuditLogger>,
    interactions: Option<&PlatformInteractionExecutor>,
    proposed_action_ids: &mut Vec<i64>,
    seen_calls: &std::collections::HashMap<String, String>,
) -> String {
    let caller = match write_handle {
        Some(c) => c,
        None => {
            return "The `temps_write` tool is not available (write caller not yet wired)."
                .to_string()
        }
    };
    let pending = match pending_svc {
        Some(p) => p,
        None => {
            return "The `temps_write` tool is not available (pending-action service absent)."
                .to_string()
        }
    };
    let interactions = match interactions {
        Some(interactions) => interactions,
        None => {
            return "The `temps_write` tool cannot request approval in this turn. No change was created or executed."
                .to_string()
        }
    };

    // Parse the JSON arguments.
    let args: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => return format!("Invalid `temps_write` arguments (not JSON): {e}"),
    };
    let requested_project_id = match args.get("project_id").and_then(serde_json::Value::as_i64) {
        Some(value) => match i32::try_from(value) {
            Ok(value) => Some(value),
            Err(_) => return "The selected project_id is outside the supported range.".to_string(),
        },
        None => None,
    };
    let application_scope = context_id.starts_with("app_") && context_id.contains(':');
    let global_scope = context_id.starts_with("global_");
    let (selected_project_id, project_scope) = if application_scope {
        // A workspace is a machine and context boundary, not an authorization
        // boundary for the Temps control plane. Project links describe which
        // source trees and data networks are mounted in this workspace. The
        // approving user's live RBAC and project membership are enforced by
        // the internal API call itself, so global resources and any currently
        // authorized project remain operable from every workspace chat.
        (
            requested_project_id,
            requested_project_id.map_or(ProjectSelectorScope::Unrestricted, |project_id| {
                ProjectSelectorScope::Allowed(vec![project_id])
            }),
        )
    } else if global_scope {
        (
            requested_project_id,
            requested_project_id.map_or(ProjectSelectorScope::Unrestricted, |project_id| {
                ProjectSelectorScope::Allowed(vec![project_id])
            }),
        )
    } else {
        if requested_project_id.is_some() && requested_project_id != project_id {
            return "Cross-project selection is available only in a workspace or global thread."
                .to_string();
        }
        (
            project_id,
            project_id.map_or(ProjectSelectorScope::Unrestricted, |project_id| {
                ProjectSelectorScope::Allowed(vec![project_id])
            }),
        )
    };
    let scope = ApiCallScope {
        auth: auth.clone(),
        project_scope,
    };

    // Two shapes: a single `command` (standalone action) or an ordered
    // `commands` array (a multi-step *plan*, confirmed one step at a time in
    // order). Use a plan when order matters — e.g. change resources THEN redeploy.
    let is_plan = args.get("commands").is_some();
    let commands: Vec<String> = if let Some(arr) = args.get("commands").and_then(|v| v.as_array()) {
        let cmds: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if cmds.is_empty() {
            return "The `temps_write` 'commands' array is empty — provide one command \
                    string per step, in execution order."
                .to_string();
        }
        cmds
    } else if let Some(c) = args.get("command").and_then(|v| v.as_str()) {
        vec![c.to_string()]
    } else {
        return "The `temps_write` tool requires either a 'command' string (one action) \
                or a 'commands' array (an ordered multi-step plan). Use `--help` to \
                discover operations."
            .to_string();
    };

    // Refuse to stage anything that should have been backtested and wasn't.
    // Checked before `prepare_write_cli` so the model gets the one instruction
    // that matters rather than a validation error it would fix and re-submit,
    // still un-backtested.
    for cmd in &commands {
        if let Some(msg) = missing_backtest(cmd, seen_calls) {
            return msg;
        }
    }

    // Prepare (validate, NO execution) every step first. If any step is a help
    // request or fails to validate, surface that and stage NOTHING — a plan is
    // only proposed once every step is valid.
    let mut prepared_steps: Vec<(temps_ai_api_tools::PreparedWrite, Option<String>)> = Vec::new();
    for (i, cmd) in commands.iter().enumerate() {
        match caller.prepare_write_cli(cmd, &scope) {
            WritePrepareOutcome::Help(text) => return text,
            WritePrepareOutcome::Invalid(msg) => {
                return if is_plan {
                    format!("Plan not staged — step {} is invalid: {msg}", i + 1)
                } else {
                    msg
                };
            }
            WritePrepareOutcome::Prepared(prepared) => {
                let perm = prepared.required_permission.clone();
                prepared_steps.push((prepared, perm));
            }
        }
    }

    // Store the encrypted action first so the approval is durable and the
    // eventual execution has an immutable, auditable request. Unlike the old
    // detached proposal flow, the active tool call now waits for the inline
    // decision and returns the execution result to the model in this turn.
    if !is_plan {
        let (prepared, perm) = &prepared_steps[0];
        let row = match pending
            .create_inline(
                conversation_id,
                selected_project_id,
                prepared,
                perm.clone(),
                auth.user_id(),
            )
            .await
        {
            Ok(row) => row,
            Err(e) => return format!("Could not prepare this change for approval: {e}"),
        };
        proposed_action_ids.push(row.id);
        let mut lifecycle = InlineActionGuard::new(pending, vec![row.public_id.clone()]);
        let request = temps_ai::streaming::PermissionRequest {
            id: uuid::Uuid::new_v4().simple().to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
            input: serde_json::json!({
                "operation": row.operation_id,
                "method": row.method,
                "summary": row.summary,
                "project_id": row.project_id,
                "parameters": redact_value(&prepared.params),
                "required_permission": perm,
                "action_id": row.public_id,
            }),
        };
        let resolution = match interactions(request).await {
            Ok(resolution) => resolution,
            Err(error) => {
                return format!(
                    "Approval for `{}` ended before it was resolved: {error}",
                    row.operation_id
                )
            }
        };
        return match resolution.decision {
            PermissionDecision::AllowTool => {
                match pending
                    .confirm_inline(
                        row.project_id,
                        &row.public_id,
                        &resolution.auth,
                        Some(resolution.auth.user_id()),
                    )
                    .await
                {
                    Ok(updated) => {
                        lifecycle.disarm();
                        audit_confirmed_action(audit, &resolution, &updated).await;
                        pending_action_tool_result(&updated)
                    }
                    Err(error) => serde_json::json!({
                        "status": "failed",
                        "action_id": row.public_id,
                        "operation": row.operation_id,
                        "error": redact_text(&error.to_string()),
                        "instruction": "The approved platform action failed. Diagnose this error, inspect current state if useful, and either explain the blocker or propose a corrected action."
                    })
                    .to_string(),
                }
            }
            PermissionDecision::DenyTool { ref reason } => {
                match pending
                    .reject_inline(
                        row.project_id,
                        &row.public_id,
                        &resolution.auth,
                        Some(resolution.auth.user_id()),
                    )
                    .await
                {
                    Ok(updated) => {
                        lifecycle.disarm();
                        audit_rejected_action(audit, &resolution, &updated).await;
                        serde_json::json!({
                            "status": "rejected",
                            "action_id": row.public_id,
                            "operation": row.operation_id,
                            "reason": reason.as_ref().map(|value| redact_text(value)),
                            "instruction": "The user rejected this action. Acknowledge the decision and use their reason when choosing a safer next step."
                        })
                        .to_string()
                    }
                    Err(error) => {
                        audit_action_transition_failed(
                            audit,
                            &resolution,
                            &row,
                            "reject",
                            &error.to_string(),
                        )
                        .await;
                        serde_json::json!({
                            "status": "failed",
                            "action_id": row.public_id,
                            "operation": row.operation_id,
                            "error": redact_text(&error.to_string()),
                            "instruction": "The rejection could not be recorded, so this action was cancelled and was not executed. Explain that the platform could not persist the decision."
                        })
                        .to_string()
                    }
                }
            }
            _ => serde_json::json!({
                "status": "failed",
                "action_id": row.public_id,
                "operation": row.operation_id,
                "error": "The approval response did not match this tool request."
            })
            .to_string(),
        };
    }

    // Multi-step plans use one inline plan approval. Approval executes the
    // immutable steps in order; the first failure halts the remainder and is
    // returned to the model so it can react without a separate user message.
    let rows = match pending
        .create_inline_plan(
            conversation_id,
            selected_project_id,
            &prepared_steps,
            auth.user_id(),
        )
        .await
    {
        Ok(rows) => rows,
        Err(e) => return format!("Could not prepare this plan for approval: {e}"),
    };
    proposed_action_ids.extend(rows.iter().map(|row| row.id));
    let mut lifecycle = InlineActionGuard::new(
        pending,
        rows.iter().map(|row| row.public_id.clone()).collect(),
    );
    let plan_text = rows
        .iter()
        .map(|row| {
            format!(
                "{}. **{}** — {}",
                row.step_index + 1,
                row.operation_id,
                row.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let request = temps_ai::streaming::PermissionRequest {
        id: uuid::Uuid::new_v4().simple().to_string(),
        kind: PermissionKind::PlanApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({
            "plan": plan_text,
            "plan_id": rows.first().and_then(|row| row.plan_public_id.clone()),
            "project_id": selected_project_id,
            "steps": rows.iter().zip(prepared_steps.iter()).map(|(row, (prepared, _))| serde_json::json!({
                "step": row.step_index + 1,
                "action_id": row.public_id,
                "operation": row.operation_id,
                "method": row.method,
                "summary": row.summary,
                "parameters": redact_value(&prepared.params),
                "required_permission": row.required_permission,
            })).collect::<Vec<_>>()
        }),
    };
    let resolution = match interactions(request).await {
        Ok(resolution) => resolution,
        Err(error) => return format!("Plan approval ended before it was resolved: {error}"),
    };
    match resolution.decision {
        PermissionDecision::ApprovePlan => {
            let mut results = Vec::with_capacity(rows.len());
            let mut transition_failed = false;
            for row in &rows {
                match pending
                    .confirm_inline(
                        row.project_id,
                        &row.public_id,
                        &resolution.auth,
                        Some(resolution.auth.user_id()),
                    )
                    .await
                {
                    Ok(updated) => {
                        audit_confirmed_action(audit, &resolution, &updated).await;
                        let failed = updated.status == "failed";
                        results.push(pending_action_result_value(&updated));
                        if failed {
                            break;
                        }
                    }
                    Err(error) => {
                        transition_failed = true;
                        results.push(serde_json::json!({
                            "status": "failed",
                            "action_id": row.public_id,
                            "operation": row.operation_id,
                            "error": redact_text(&error.to_string()),
                        }));
                        break;
                    }
                }
            }
            // `confirm_inline` terminally transitions every attempted row and
            // skips the remainder after the first failure.
            if !transition_failed {
                lifecycle.disarm();
            }
            let failed = results.iter().any(|result| {
                result.get("status").and_then(serde_json::Value::as_str) == Some("failed")
            });
            serde_json::json!({
                "status": if failed { "failed" } else { "executed" },
                "plan_id": rows.first().and_then(|row| row.plan_public_id.clone()),
                "steps": results,
                "instruction": if failed {
                    "The approved plan halted because a step failed. Diagnose the returned error and propose only the correction that is still needed."
                } else {
                    "The approved plan executed successfully. Summarize the concrete outcome."
                }
            })
            .to_string()
        }
        PermissionDecision::RejectPlan { ref feedback } => {
            let rejection = if let Some(first) = rows.first() {
                pending
                    .reject_inline(
                        first.project_id,
                        &first.public_id,
                        &resolution.auth,
                        Some(resolution.auth.user_id()),
                    )
                    .await
                    .map(Some)
            } else {
                Ok(None)
            };
            match rejection {
                Ok(updated) => {
                    lifecycle.disarm();
                    if let Some(updated) = &updated {
                        audit_rejected_action(audit, &resolution, updated).await;
                    }
                    serde_json::json!({
                        "status": "rejected",
                        "plan_id": rows.first().and_then(|row| row.plan_public_id.clone()),
                        "feedback": feedback.as_ref().map(|value| redact_text(value)),
                        "instruction": "The user rejected this plan. Acknowledge their feedback and do not execute or restage it unchanged."
                    })
                    .to_string()
                }
                Err(error) => {
                    if let Some(first) = rows.first() {
                        audit_action_transition_failed(
                            audit,
                            &resolution,
                            first,
                            "reject_plan",
                            &error.to_string(),
                        )
                        .await;
                    }
                    serde_json::json!({
                        "status": "failed",
                        "plan_id": rows.first().and_then(|row| row.plan_public_id.clone()),
                        "error": redact_text(&error.to_string()),
                        "instruction": "The plan rejection could not be recorded. The inline actions were cancelled and none were executed. Explain the persistence failure."
                    })
                    .to_string()
                }
            }
        }
        _ => serde_json::json!({
            "status": "failed",
            "error": "The approval response did not match this plan request."
        })
        .to_string(),
    }
}

fn pending_action_result_value(
    action: &temps_entities::ai_pending_actions::Model,
) -> serde_json::Value {
    serde_json::json!({
        "status": action.status,
        "action_id": action.public_id,
        "operation": action.operation_id,
        "method": action.method,
        "summary": action.summary,
        "result": action.result,
        "error": action.error,
    })
}

fn pending_action_tool_result(action: &temps_entities::ai_pending_actions::Model) -> String {
    let mut value = pending_action_result_value(action);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "instruction".to_string(),
            serde_json::Value::String(if action.status == "failed" {
                "The approved platform action failed. Diagnose the returned error, inspect current state if useful, and either explain the blocker or propose a corrected action."
            } else {
                "The approved platform action executed successfully. Summarize the concrete outcome."
            }.to_string()),
        );
    }
    value.to_string()
}

async fn audit_confirmed_action(
    audit: Option<&dyn AuditLogger>,
    resolution: &PermissionResolution,
    action: &temps_entities::ai_pending_actions::Model,
) {
    let Some(audit) = audit else { return };
    let event = AiActionConfirmedAudit {
        context: AuditContext {
            user_id: resolution.auth.user_id(),
            ip_address: Some(resolution.metadata.ip_address.clone()),
            user_agent: resolution.metadata.user_agent.clone(),
        },
        project_id: action.project_id,
        action_id: action.public_id.clone(),
        operation_id: action.operation_id.clone(),
        status: action.status.clone(),
    };
    if let Err(error) = audit.create_audit_log(&event).await {
        tracing::error!(action_id = %action.public_id, "failed to audit inline AI action: {error}");
    }
}

async fn audit_rejected_action(
    audit: Option<&dyn AuditLogger>,
    resolution: &PermissionResolution,
    action: &temps_entities::ai_pending_actions::Model,
) {
    let Some(audit) = audit else { return };
    let event = AiActionRejectedAudit {
        context: AuditContext {
            user_id: resolution.auth.user_id(),
            ip_address: Some(resolution.metadata.ip_address.clone()),
            user_agent: resolution.metadata.user_agent.clone(),
        },
        project_id: action.project_id,
        action_id: action.public_id.clone(),
        operation_id: action.operation_id.clone(),
    };
    if let Err(error) = audit.create_audit_log(&event).await {
        tracing::error!(action_id = %action.public_id, "failed to audit rejected inline AI action: {error}");
    }
}

async fn audit_action_transition_failed(
    audit: Option<&dyn AuditLogger>,
    resolution: &PermissionResolution,
    action: &temps_entities::ai_pending_actions::Model,
    attempted_transition: &str,
    error: &str,
) {
    let Some(audit) = audit else { return };
    let event = AiActionTransitionFailedAudit {
        context: AuditContext {
            user_id: resolution.auth.user_id(),
            ip_address: Some(resolution.metadata.ip_address.clone()),
            user_agent: resolution.metadata.user_agent.clone(),
        },
        project_id: action.project_id,
        action_id: action.public_id.clone(),
        operation_id: action.operation_id.clone(),
        attempted_transition: attempted_transition.to_string(),
        error: redact_text(error),
    };
    if let Err(audit_error) = audit.create_audit_log(&event).await {
        tracing::error!(
            action_id = %action.public_id,
            %audit_error,
            "failed to audit inline AI action transition failure"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_provider_sessions_are_the_only_resume_errors_retried() {
        assert!(provider_resume_session_is_missing(
            "claude_cli",
            "No conversation found with session ID: old-session"
        ));
        assert!(provider_resume_session_is_missing(
            "codex_cli",
            "Thread not found for id old-session"
        ));
        assert!(provider_resume_session_is_missing(
            "opencode",
            "Session not found: old-session"
        ));

        for reason in [
            "Token refresh failed: 401",
            "Model sonnet is unavailable",
            "process exited with code 1",
        ] {
            assert!(
                !provider_resume_session_is_missing("claude_cli", reason),
                "must preserve unrelated provider failure: {reason}"
            );
        }
    }

    #[test]
    fn changing_models_starts_a_fresh_cli_session() {
        assert_eq!(
            cli_session_after_model_change("sonnet", "sonnet", Some("session-1")).as_deref(),
            Some("session-1")
        );
        assert_eq!(
            cli_session_after_model_change("opus[1m]", "sonnet", Some("session-1")),
            None
        );
        assert_eq!(
            cli_session_fingerprint_after_model_change("sonnet", "sonnet", Some("v1:fingerprint"))
                .as_deref(),
            Some("v1:fingerprint")
        );
        assert_eq!(
            cli_session_fingerprint_after_model_change(
                "opus[1m]",
                "sonnet",
                Some("v1:fingerprint")
            ),
            None
        );
    }

    use std::collections::HashMap;

    #[test]
    fn productive_results_are_distinguished_from_rejections() {
        // The read CLI's success shape — a contract, not a guess.
        assert!(tool_result_is_productive(
            r#"{"operation":"query_metrics","status":200,"data":{"data":[]}}"#
        ));
        // A non-2xx replayed through the CLI is not progress.
        assert!(!tool_result_is_productive(
            r#"{"operation":"get_alert","status":404,"data":null}"#
        ));
        // The write tool's proposal receipt has no status and is very much progress.
        assert!(tool_result_is_productive(
            r#"{"status":"proposed","action_id":"abc","operation":"create_alert"}"#
        ));

        for rejection in [
            "Unknown parameter(s) for operation 'query_metrics': 'metric'.",
            "Missing required parameters for operation 'create_alert': name.",
            "Parameter 'detection_config' has the wrong shape for operation 'create_alert'.",
            "`query_metrics` failed: something went wrong",
            "You already ran `echo` with these exact arguments earlier this turn.",
            "Not staged: `create_alert` requires a backtest first.",
        ] {
            assert!(
                !tool_result_is_productive(rejection),
                "should count as unproductive: {rejection}"
            );
        }

        // An unrecognised tool's prose must NOT be mistaken for a failure —
        // misclassifying it would cut a working turn short.
        assert!(tool_result_is_productive(
            "The deployment finished at 12:04 and the container is healthy."
        ));
    }

    fn inline_action(
        status: &str,
        error: Option<&str>,
    ) -> temps_entities::ai_pending_actions::Model {
        temps_entities::ai_pending_actions::Model {
            id: 1,
            public_id: "action-1".to_string(),
            conversation_id: 1,
            message_id: None,
            project_id: Some(7),
            plan_public_id: None,
            step_index: 0,
            operation_id: "create_service".to_string(),
            method: "POST".to_string(),
            summary: "Create service".to_string(),
            params: serde_json::json!({}),
            required_permission: Some("projects:write".to_string()),
            status: status.to_string(),
            result: (status == "executed").then(|| serde_json::json!({"id": 42})),
            error: error.map(str::to_string),
            created_by: 1,
            confirmed_by: Some(1),
            created_at: Utc::now(),
            confirmed_at: Some(Utc::now()),
            executed_at: (status == "executed").then(Utc::now),
        }
    }

    #[test]
    fn inline_write_failure_becomes_model_visible_tool_evidence() {
        let result = pending_action_tool_result(&inline_action(
            "failed",
            Some("upstream rejected the requested database version"),
        ));
        let value: serde_json::Value = serde_json::from_str(&result).expect("valid tool JSON");
        assert_eq!(value["status"], "failed");
        assert_eq!(value["operation"], "create_service");
        assert!(value["error"]
            .as_str()
            .is_some_and(|error| error.contains("database version")));
        assert!(value["instruction"]
            .as_str()
            .is_some_and(|instruction| instruction.contains("Diagnose")));
    }

    #[test]
    fn inline_write_success_is_not_reported_as_a_detached_proposal() {
        let result = pending_action_tool_result(&inline_action("executed", None));
        let value: serde_json::Value = serde_json::from_str(&result).expect("valid tool JSON");
        assert_eq!(value["status"], "executed");
        assert_eq!(value["result"]["id"], 42);
        assert_ne!(value["status"], "proposed");
    }

    /// What is retained for recall must be bounded, or a turn holds every tool
    /// result at full size in memory while the transcript trimming is busy
    /// bounding the very same data.
    #[test]
    fn recall_copies_are_capped() {
        let small = "a short result";
        assert_eq!(summarize_for_recall(small), small);

        let huge = "x".repeat(200_000);
        let kept = summarize_for_recall(&huge);
        assert!(kept.len() < 5 * 1024, "not capped: {} bytes", kept.len());
        assert!(kept.contains("truncated for recall"));
    }

    /// Tool output is arbitrary text, so the cap has to land on a character
    /// boundary — slicing blind panics on a multi-byte character across it.
    #[test]
    fn recall_cap_does_not_split_a_multibyte_character() {
        // 'é' is two bytes; a run of them guarantees a boundary at the cut.
        let multibyte = "é".repeat(100_000);
        let kept = summarize_for_recall(&multibyte);
        assert!(kept.contains("truncated for recall"));
    }

    /// The gate must read the operation in command position, not anywhere in
    /// the string — an alert *named* `create_alert` must not make an unrelated
    /// call demand a backtest.
    #[test]
    fn backtest_gate_ignores_operation_names_inside_arguments() {
        let seen = HashMap::new();
        assert!(
            missing_backtest(
                "deployments trigger_project_pipeline --name create_alert",
                &seen
            )
            .is_none(),
            "an operation name in an argument must not trip the gate"
        );
        // ...while the real thing still does, with or without a section prefix.
        assert!(missing_backtest("alerts create_alert --metric_name x", &seen).is_some());
        assert!(missing_backtest("create_alert --metric_name x", &seen).is_some());
    }

    #[test]
    fn carried_tool_results_are_trimmed_oldest_first() {
        let big = "x".repeat(1000);
        let mut messages = vec![
            ChatMessage::system("seed"),
            ChatMessage::tool("a".to_string(), big.clone()),
            ChatMessage::tool("b".to_string(), big.clone()),
            ChatMessage::tool("c".to_string(), big.clone()),
        ];

        trim_carried_tool_results(&mut messages, 1500);

        // Structure is preserved — a provider rejects a tool_calls block whose
        // answers have vanished, so messages are stubbed in place, never removed.
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("a"));

        // Oldest went first; the newest result — what the model is actually
        // reasoning about — survives intact.
        assert!(messages[1].content.contains("dropped"));
        assert_eq!(messages[3].content, big);

        let carried: usize = messages
            .iter()
            .filter(|m| m.role == "tool")
            .map(|m| m.content.len())
            .sum();
        assert!(carried <= 1500, "still over budget: {carried}");
    }

    #[test]
    fn trimming_leaves_a_transcript_under_budget_alone() {
        let mut messages = vec![
            ChatMessage::tool("a".to_string(), "small".to_string()),
            ChatMessage::tool("b".to_string(), "also small".to_string()),
        ];
        let before = messages.clone();
        trim_carried_tool_results(&mut messages, 192 * 1024);
        assert_eq!(messages[0].content, before[0].content);
        assert_eq!(messages[1].content, before[1].content);
    }

    /// A `create_alert` proposal with no prior backtest must be refused.
    ///
    /// The system prompt asks for a backtest in the strongest terms available
    /// and a live model still skipped it — then wrote "backtested via
    /// preview_alert" in its answer without having called it. A threshold
    /// nobody checked is a guess, so this is enforced rather than requested.
    #[test]
    fn create_alert_without_a_backtest_is_refused() {
        let seen = HashMap::new();
        let msg = missing_backtest(
            "alerts create_alert --metric_name http.server.duration --severity critical",
            &seen,
        )
        .expect("must refuse an un-backtested proposal");

        assert!(msg.contains("preview_alert"), "must name the tool: {msg}");
        assert!(
            msg.contains("Not staged"),
            "must be clear nothing was proposed: {msg}"
        );
    }

    /// A successful backtest earlier in the turn satisfies the requirement.
    #[test]
    fn create_alert_after_a_backtest_is_allowed() {
        let mut seen = HashMap::new();
        seen.insert(
            "temps|{\"command\":\"alerts preview_alert --metric_name http.server.duration\"}"
                .to_string(),
            "{\"operation\":\"preview_alert\",\"status\":200,\"data\":{\"breach_count\":22}}"
                .to_string(),
        );
        assert!(missing_backtest(
            "alerts create_alert --metric_name http.server.duration",
            &seen
        )
        .is_none());
    }

    /// A backtest that errored proves nothing, so it must not unlock the
    /// proposal — otherwise a single malformed call becomes a way past the gate.
    #[test]
    fn a_failed_backtest_does_not_satisfy_the_requirement() {
        let mut seen = HashMap::new();
        seen.insert(
            "temps|{\"command\":\"alerts preview_alert --metric foo\"}".to_string(),
            "`preview_alert` failed: Unknown parameter(s) for operation 'preview_alert': 'metric'"
                .to_string(),
        );
        assert!(missing_backtest("alerts create_alert --metric_name foo", &seen).is_some());

        // ...and a non-2xx from the real operation is not a backtest either.
        let mut seen = HashMap::new();
        seen.insert(
            "k".to_string(),
            "{\"operation\":\"preview_alert\",\"status\":400,\"data\":null}".to_string(),
        );
        assert!(missing_backtest("alerts create_alert --metric_name foo", &seen).is_some());
    }

    /// The gate must judge what RAN, not what was asked for.
    ///
    /// `preview_alert --help` names the operation and returns friendly text. The
    /// original implementation matched on the argument string, so help counted
    /// as a backtest: the model could propose a threshold it never verified
    /// while the human confirming saw it described as checked. Found by an
    /// independent security audit, not by the author.
    #[test]
    fn help_output_does_not_satisfy_the_backtest_requirement() {
        let mut seen = HashMap::new();
        seen.insert(
            "temps|{\"command\":\"alerts preview_alert --help\"}".to_string(),
            "preview_alert — POST /otel/alerts/preview\nBacktest an anomaly detector.\nFlags:\n  \
             --metric_name <string>"
                .to_string(),
        );
        assert!(
            missing_backtest("alerts create_alert --metric_name x", &seen).is_some(),
            "help text must never count as having run the backtest"
        );
    }

    /// Nor may a different operation that merely mentions it in its arguments.
    #[test]
    fn another_operation_mentioning_the_backtest_does_not_satisfy_it() {
        let mut seen = HashMap::new();
        seen.insert(
            "temps|{\"command\":\"alerts list_alerts --name preview_alert\"}".to_string(),
            "{\"operation\":\"list_alerts\",\"status\":200,\"data\":{\"data\":[]}}".to_string(),
        );
        assert!(missing_backtest("alerts create_alert --metric_name x", &seen).is_some());
    }

    /// Everything else stays unaffected — this gate is about thresholds, not
    /// write actions in general.
    #[test]
    fn other_write_operations_need_no_backtest() {
        let seen = HashMap::new();
        for cmd in [
            "deployments trigger_project_pipeline --environment_id 8",
            "containers restart_container --id abc",
            "environments update_environment_settings --replicas 2",
        ] {
            assert!(
                missing_backtest(cmd, &seen).is_none(),
                "{cmd} must not require a backtest"
            );
        }
    }

    #[test]
    fn clean_title_strips_quotes_and_punctuation() {
        assert_eq!(clean_title("\"Recent Audit Logs.\""), "Recent Audit Logs");
        assert_eq!(
            clean_title("  Deploy Failure Investigation!  "),
            "Deploy Failure Investigation"
        );
    }

    #[test]
    fn clean_title_keeps_first_nonempty_line() {
        assert_eq!(
            clean_title("\n\n  Fetch Audit Logs\nextra line"),
            "Fetch Audit Logs"
        );
        assert_eq!(clean_title("Fetch Audit Logs\nextra"), "Fetch Audit Logs");
    }

    #[test]
    fn clean_title_collapses_whitespace_and_caps_length() {
        assert_eq!(clean_title("Get   last    20  logs"), "Get last 20 logs");
        let long = "word ".repeat(40);
        assert!(clean_title(&long).chars().count() <= TITLE_MAX_CHARS);
    }

    #[test]
    fn clean_title_empty_input_is_empty() {
        assert_eq!(clean_title("   \n  "), "");
    }

    use std::sync::Mutex;

    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    use temps_ai::{
        AiError, AiRequest, AiResponse, ChatStreamDelta, ChatTurnStream, TokenStream, ToolCall,
    };

    #[test]
    fn assistant_draft_keeps_every_tool_and_interleaved_text_segment() {
        let mut tools = Vec::new();
        let mut parts = vec![serde_json::json!({
            "type": "text",
            "text": "Scaffolded the app."
        })];
        record_tool_call(
            &mut tools,
            &mut parts,
            "tool-1",
            "Bash",
            r#"{"command":"npm create next-app"}"#,
        );
        record_tool_result(
            &mut tools,
            &mut parts,
            "tool-1",
            "Bash",
            r#"{"command":"npm create next-app"}"#,
            "Created landing-page",
        );
        record_tool_call(
            &mut tools,
            &mut parts,
            "tool-2",
            "Bash",
            r#"{"command":"npx shadcn init"}"#,
        );

        let metadata = assistant_message_metadata(&tools, &parts, true)
            .expect("a draft with tool calls has metadata");
        let persisted_tools = metadata["tools"].as_array().expect("tools array");
        let persisted_parts = metadata["parts"].as_array().expect("parts array");

        assert_eq!(persisted_tools.len(), 2);
        assert_eq!(persisted_tools[0]["result"], "Created landing-page");
        assert!(persisted_tools[1]["result"].is_null());
        assert_eq!(persisted_parts.len(), 3);
        assert_eq!(persisted_parts[0]["text"], "Scaffolded the app.");
        assert_eq!(metadata["draft"], true);
    }

    #[tokio::test]
    async fn active_assistant_checkpoint_is_written_to_the_message_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let tools = vec![serde_json::json!({
            "id": "tool-1",
            "name": "Bash",
            "arguments": "{}",
            "result": "done"
        })];
        let parts = vec![serde_json::json!({
            "type": "text",
            "text": "Before approval."
        })];

        persist_assistant_message(
            &db,
            42,
            "Before approval. Continuing after approval.",
            &tools,
            &parts,
            " Continuing after approval.",
            true,
        )
        .await
        .expect("checkpoint persists");

        let log = db.into_transaction_log();
        let statements = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect::<Vec<_>>();
        assert_eq!(statements.len(), 1);
        assert!(statements[0].sql.contains("UPDATE \"ai_messages\""));
        let statement = format!("{:?}", statements[0]);
        assert!(statement.contains("Before approval. Continuing after approval."));
        assert!(statement.contains("tool-1"));
        assert!(statement.contains("draft"));
    }

    /// A scripted `AiService`: each `chat_stream_turn` call pops the next queued
    /// round (a list of [`ChatStreamDelta`]s to stream, or an error to fail the
    /// call with) so a test can drive the agentic loop round-by-round, while
    /// counting how many model calls were made.
    struct ScriptedAi {
        /// Front-to-back queue of rounds for successive `chat_stream_turn` calls.
        rounds: Mutex<std::collections::VecDeque<Result<Vec<ChatStreamDelta>, AiError>>>,
        /// Counts `chat_stream_turn` invocations (kept named `chat_calls` for the
        /// round-cap assertions).
        chat_calls: Arc<std::sync::atomic::AtomicUsize>,
        available: bool,
        /// Advance the paused test clock by this much on every model call, so a
        /// deadline can be exercised without a test that actually waits.
        advance_per_round: Option<std::time::Duration>,
    }

    impl ScriptedAi {
        fn new(rounds: Vec<Result<Vec<ChatStreamDelta>, AiError>>) -> Self {
            Self {
                rounds: Mutex::new(rounds.into_iter().collect()),
                chat_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                available: true,
                advance_per_round: None,
            }
        }

        /// Advance the paused clock by `d` on each model call.
        fn advancing(mut self, d: std::time::Duration) -> Self {
            self.advance_per_round = Some(d);
            self
        }

        #[allow(dead_code)]
        fn unavailable() -> Self {
            Self {
                available: false,
                ..Self::new(vec![])
            }
        }
    }

    #[async_trait]
    impl AiService for ScriptedAi {
        async fn is_available(&self) -> bool {
            self.available
        }
        async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
            Err(AiError::NotAvailable)
        }
        async fn capabilities_for(
            &self,
            provider: Option<&str>,
            _refresh: temps_ai::RefreshPolicy,
        ) -> Result<temps_ai::ProviderCapabilities, AiError> {
            Ok(temps_ai::ProviderCapabilities {
                id: provider.unwrap_or("gateway_key:1").to_string(),
                name: "Test provider".to_string(),
                auth_source: temps_ai::ProviderAuthSource::ConfiguredKey,
                models: vec![
                    temps_ai::ModelCapability {
                        id: "gpt-4o-mini".to_string(),
                        name: "GPT-4o mini".to_string(),
                        thinking_modes: vec![temps_ai::SelectOption {
                            id: "high".to_string(),
                            name: "High".to_string(),
                            description: None,
                        }],
                        tool_thinking_modes: None,
                        default_thinking_mode_id: None,
                    },
                    temps_ai::ModelCapability {
                        id: "gpt-4.1".to_string(),
                        name: "GPT-4.1".to_string(),
                        thinking_modes: vec![temps_ai::SelectOption {
                            id: "low".to_string(),
                            name: "Low".to_string(),
                            description: None,
                        }],
                        tool_thinking_modes: None,
                        default_thinking_mode_id: Some("low".to_string()),
                    },
                    temps_ai::ModelCapability {
                        id: "gpt-5.6-luna".to_string(),
                        name: "GPT-5.6 Luna".to_string(),
                        thinking_modes: vec![
                            temps_ai::SelectOption {
                                id: "none".to_string(),
                                name: "None".to_string(),
                                description: None,
                            },
                            temps_ai::SelectOption {
                                id: "medium".to_string(),
                                name: "Medium".to_string(),
                                description: None,
                            },
                        ],
                        tool_thinking_modes: None,
                        default_thinking_mode_id: Some("medium".to_string()),
                    },
                ],
                default_model_id: Some("gpt-4o-mini".to_string()),
                permission_modes: vec![
                    temps_ai::SelectOption {
                        id: "confirm-actions".to_string(),
                        name: "Confirm actions".to_string(),
                        description: None,
                    },
                    temps_ai::SelectOption {
                        id: "full-access".to_string(),
                        name: "Full access".to_string(),
                        description: None,
                    },
                ],
                default_permission_mode_id: Some("confirm-actions".to_string()),
                realtime: temps_ai::RealtimeCapabilities {
                    text_streaming: true,
                    reasoning_streaming: false,
                    tool_events: true,
                    user_interactions: true,
                    cancellation: true,
                },
            })
        }
        async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
            Err(AiError::NotAvailable)
        }
        async fn chat_stream_turn(
            &self,
            _request: ChatTurnRequest,
        ) -> Result<ChatTurnStream, AiError> {
            self.chat_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(d) = self.advance_per_round {
                tokio::time::advance(d).await;
            }
            // When the script is exhausted, keep requesting the same tool call so a
            // misbehaving loop would run forever — letting MAX_ROUNDS assert.
            let round = self
                .rounds
                .lock()
                .expect("scripted-ai lock")
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                        id: "loop".to_string(),
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    })])
                });
            let deltas = round?;
            let s = async_stream::stream! {
                for d in deltas {
                    yield Ok(d);
                }
            };
            Ok(Box::pin(s))
        }
    }

    /// Models the common local-development setup: no gateway key is configured
    /// while an authenticated host harness is ready for chat.
    struct HostHarnessOnlyAi;

    #[async_trait]
    impl AiService for HostHarnessOnlyAi {
        async fn is_available(&self) -> bool {
            false
        }

        async fn chat_capable_for(&self, provider: Option<&str>) -> bool {
            provider == Some("claude_cli")
        }

        async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
            Err(AiError::NotAvailable)
        }

        async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
            Err(AiError::NotAvailable)
        }
    }

    /// A stub provider exposing a single `echo` tool, counting executions.
    struct StubProvider {
        tool_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    struct AuthRecordingProvider {
        seen_user_id: Arc<std::sync::atomic::AtomicI32>,
    }

    struct SeedOnlyProvider;

    struct RevokedProjectAccessChecker;

    struct ReadOnlyProjectAccessChecker;

    #[async_trait]
    impl temps_core::ProjectAccessChecker for RevokedProjectAccessChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }
    }

    #[async_trait]
    impl temps_core::ProjectAccessChecker for ReadOnlyProjectAccessChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(true)
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Some(vec![
                temps_auth::permissions::Permission::ProjectsRead.to_string(),
            ]))
        }
    }

    #[async_trait]
    impl ConversationContextProvider for StubProvider {
        fn context_type(&self) -> &'static str {
            "test"
        }
        async fn seed(
            &self,
            _project_id: Option<i32>,
            _context_id: &str,
        ) -> Option<crate::provider::ConversationSeed> {
            None
        }
        async fn tools(&self, _project_id: Option<i32>, _context_id: &str) -> Vec<ChatTool> {
            vec![ChatTool {
                name: "echo".to_string(),
                description: "Echoes its input.".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }]
        }
        async fn execute_tool(
            &self,
            _project_id: Option<i32>,
            _context_id: &str,
            _name: &str,
            _arguments: &str,
        ) -> String {
            self.tool_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            "tool result".to_string()
        }
    }

    #[async_trait]
    impl ConversationContextProvider for AuthRecordingProvider {
        fn context_type(&self) -> &'static str {
            "__api_tools__"
        }

        async fn seed(
            &self,
            _project_id: Option<i32>,
            _context_id: &str,
        ) -> Option<crate::provider::ConversationSeed> {
            None
        }

        async fn execute_tool_with_auth(
            &self,
            _project_id: Option<i32>,
            _context_id: &str,
            _name: &str,
            _arguments: &str,
            auth: &AuthContext,
        ) -> String {
            self.seen_user_id
                .store(auth.user_id(), std::sync::atomic::Ordering::SeqCst);
            "authenticated result".to_string()
        }
    }

    #[async_trait]
    impl ConversationContextProvider for SeedOnlyProvider {
        fn context_type(&self) -> &'static str {
            "deployment"
        }

        async fn seed(
            &self,
            _project_id: Option<i32>,
            _context_id: &str,
        ) -> Option<crate::provider::ConversationSeed> {
            Some(crate::provider::ConversationSeed {
                system: "private system context".to_string(),
                first_assistant: None,
                title: Some("Private chat".to_string()),
                metadata: None,
            })
        }
    }

    fn test_conversation() -> ai_conversations::Model {
        let now = Utc::now();
        ai_conversations::Model {
            id: 1,
            public_id: "pub1".to_string(),
            project_id: Some(7),
            application_id: None,
            context_type: "test".to_string(),
            context_id: "42".to_string(),
            title: None,
            status: "active".to_string(),
            created_by: 1,
            metadata: None,
            cli_session_id: None,
            cli_session_fingerprint: None,
            ai_provider: "gateway".to_string(),
            ai_model: "gpt-4o-mini".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "confirm-actions".to_string(),
            turn_status: "idle".to_string(),
            active_turn_id: None,
            last_turn_id: None,
            turn_started_at: None,
            created_at: now,
            last_activity_at: now,
        }
    }

    fn test_auth() -> AuthContext {
        let now = Utc::now();
        let user = temps_entities::users::Model {
            id: 1,
            name: "tester".to_string(),
            email: "tester@internal".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        };
        AuthContext::new_session(user, temps_auth::permissions::Role::Admin)
    }

    fn test_request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "temps-ai-chat-test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    fn auth_with_role(role: temps_auth::permissions::Role) -> AuthContext {
        let mut auth = test_auth();
        auth.effective_role = role;
        auth
    }

    /// The chat endpoints require only `ProjectsRead`/`ProjectsWrite`, but the
    /// repo tools read private source through the project's stored Git token
    /// and stream it back verbatim in a `tool_result` frame. Without the
    /// repository permission the caller must not reach them.
    #[test]
    fn repo_tools_require_the_git_repository_read_permission() {
        use temps_auth::permissions::{Permission, Role};

        // Admin holds everything, including GitRepositoriesRead.
        assert!(caller_may_use_repo_tools(&auth_with_role(Role::Admin)));

        // A role that can read projects but not repositories must be refused —
        // otherwise `projects:read` silently grants source-code access.
        let reader = auth_with_role(Role::Reader);
        if !reader.has_permission(&Permission::GitRepositoriesRead) {
            assert!(
                !caller_may_use_repo_tools(&reader),
                "a caller without {} must not reach the repo tools",
                Permission::GitRepositoriesRead
            );
        }

        // A custom API key scoped to projects only is the concrete case from
        // the report: ProjectsWrite, no Git permission at all.
        let mut custom = auth_with_role(Role::Custom);
        custom.custom_permissions = Some(vec![Permission::ProjectsRead, Permission::ProjectsWrite]);
        assert!(!caller_may_use_repo_tools(&custom));

        // The same key with the repository permission added is allowed.
        custom.custom_permissions = Some(vec![
            Permission::ProjectsRead,
            Permission::ProjectsWrite,
            Permission::GitRepositoriesRead,
        ]);
        assert!(caller_may_use_repo_tools(&custom));
    }

    #[tokio::test]
    async fn revoked_project_membership_denies_repo_tool_offer_and_execution() {
        use temps_auth::permissions::{Permission, Role};

        let mut auth = auth_with_role(Role::Custom);
        auth.custom_permissions = Some(vec![Permission::GitRepositoriesRead]);
        let checker: Arc<dyn temps_core::ProjectAccessChecker> =
            Arc::new(RevokedProjectAccessChecker);

        assert!(
            !project_repo_tools_allowed(&auth, Some(7), Some(&checker)).await,
            "a former project member must not be offered private repository tools"
        );

        let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
            tool_calls: tool_calls.clone(),
        });
        let mut execution = ToolExecutionState::default();
        let result = dispatch_conversation_tool(
            &ToolCall {
                id: "revoked-repo-read".to_string(),
                name: "read_repo_file".to_string(),
                arguments: r#"{"path":"private.txt"}"#.to_string(),
            },
            Some(7),
            1,
            "42",
            &auth,
            Some(&checker),
            None,
            None,
            Some(&provider),
            None,
            None,
            None,
            None,
            &mut execution,
        )
        .await;

        assert!(result.contains("current project access"), "{result}");
        assert_eq!(
            tool_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "dispatch must re-check live access before invoking the repository provider"
        );
    }

    #[tokio::test]
    async fn project_role_without_git_read_denies_repo_tools() {
        use temps_auth::permissions::{Permission, Role};

        let mut auth = auth_with_role(Role::Custom);
        auth.custom_permissions = Some(vec![Permission::GitRepositoriesRead]);
        let checker: Arc<dyn temps_core::ProjectAccessChecker> =
            Arc::new(ReadOnlyProjectAccessChecker);

        assert!(
            !project_repo_tools_allowed(&auth, Some(7), Some(&checker)).await,
            "project membership must not restore Git access removed by the project role"
        );
    }

    fn assistant_msg_model() -> ai_messages::Model {
        ai_messages::Model {
            id: 1,
            conversation_id: 1,
            role: "assistant".to_string(),
            content: "final answer".to_string(),
            metadata: None,
            tokens_in: None,
            tokens_out: None,
            cost_microcents: None,
            created_at: Utc::now(),
        }
    }

    /// Build a service whose only DB interaction (the final assistant insert) is
    /// satisfied by one mocked query result, plus the `echo` tool list to drive
    /// the loop. The provider is passed directly to `try_tool_loop` per test.
    fn service_with(ai: Arc<dyn AiService>) -> (ConversationService, Vec<ChatTool>) {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![assistant_msg_model()]])
            .into_connection();
        let tools = vec![ChatTool {
            name: "echo".to_string(),
            description: "Echoes its input.".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let svc = ConversationService {
            db: Arc::new(db),
            ai,
            providers: HashMap::new(),
            write_support: None,
            config: None,
            application_workspaces: None,
            application_sandboxes: None,
            application_service: None,
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
            harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
        };
        (svc, tools)
    }

    async fn drain(
        stream: Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>>,
    ) -> Vec<ChatStreamEvent> {
        let mut s = stream;
        let mut out = Vec::new();
        while let Some(item) = s.next().await {
            if let Ok(ev) = item {
                out.push(ev);
            }
        }
        out
    }

    struct StreamErrorAi;

    #[async_trait]
    impl AiService for StreamErrorAi {
        async fn is_available(&self) -> bool {
            true
        }

        async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
            Err(AiError::NotAvailable)
        }

        async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
            Err(AiError::NotAvailable)
        }

        async fn chat_stream_turn(
            &self,
            _request: ChatTurnRequest,
        ) -> Result<ChatTurnStream, AiError> {
            let stream = futures::stream::iter(vec![Err(AiError::Provider {
                purpose: "chat.test.tools".to_string(),
                reason: "Token refresh failed: 401".to_string(),
            })]);
            Ok(Box::pin(stream))
        }
    }

    /// Concatenate every `Token` event's text, in order.
    fn joined_text(events: &[ChatStreamEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn public_tool_results_hide_privileged_read_data() {
        assert_eq!(
            public_tool_result("read_repo_file", "SECRET_TOKEN=abc123"),
            "Tool completed; detailed result is withheld from the chat transcript."
        );
        assert_eq!(
            public_tool_result(TEMPS_WRITE_TOOL_NAME, r#"{"status":"proposed"}"#),
            r#"{"status":"proposed"}"#
        );
        assert_eq!(
            public_tool_result("mcp__temps-chat__temps_write", r#"{"status":"proposed"}"#),
            r#"{"status":"proposed"}"#
        );
    }

    #[test]
    fn proposal_claim_requires_a_fresh_write_receipt_from_this_turn() {
        let qualified_receipt = serde_json::json!({
            "id": "write-1",
            "name": "mcp__temps-chat__temps_write",
            "arguments": "{}",
            "result": r#"{"status":"proposed","action_id":"action-1"}"#,
        });
        let rejected_attempt = serde_json::json!({
            "id": "write-2",
            "name": "temps_write",
            "arguments": "{}",
            "result": "Could not stage this change: invalid version",
        });

        assert!(claims_proposal_was_staged(
            "Proposal staged — not executed. Please confirm it in the UI."
        ));
        assert!(!claims_proposal_was_staged(
            "The phrase ‘proposal staged’ means that no action has run yet."
        ));
        assert!(has_fresh_proposal_receipt(&[qualified_receipt]));
        assert!(!has_fresh_proposal_receipt(&[rejected_attempt]));
        assert!(!has_fresh_proposal_receipt(&[serde_json::json!({
            "id": "read-1",
            "name": "untrusted__temps_write",
            "result": r#"{"status":"proposed","action_id":"fake"}"#,
        })]));
    }

    #[tokio::test]
    async fn unbacked_proposal_claim_fails_the_turn_instead_of_implying_a_card_exists() {
        let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![ChatStreamDelta::Text(
            "Proposal staged — not executed. Please confirm it in the UI.".to_string(),
        )])]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let (svc, tools) = service_with(ai);
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let mut stream = svc
            .try_tool_loop(
                &test_conversation(),
                vec![],
                Some(provider_dyn),
                tools,
                &test_auth(),
            )
            .await;

        let mut errors = Vec::new();
        while let Some(item) = stream.next().await {
            if let Err(error) = item {
                errors.push(error);
            }
        }

        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ChatError::ProposalNotStaged));
        let failure = errors[0].public_failure();
        assert_eq!(failure.code, "proposal_not_staged");
        assert!(failure.detail.contains("no approval card exists"));
        assert!(failure.detail.contains("no change was made"));
    }

    #[tokio::test]
    async fn first_application_turn_persists_and_publishes_harness_session_title() {
        let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![
            ChatStreamDelta::SessionMetadata {
                session_id: Some("session-1".to_string()),
                title: Some("Create MongoDB Instance".to_string()),
            },
            ChatStreamDelta::Text("Ready.".to_string()),
        ])]));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
            ])
            .append_query_results(vec![vec![assistant_msg_model()]])
            .into_connection();
        let svc = ConversationService {
            db: Arc::new(db),
            ai,
            providers: HashMap::new(),
            write_support: None,
            config: None,
            application_workspaces: None,
            application_sandboxes: None,
            application_service: None,
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
            harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
        };
        let mut conversation = test_conversation();
        conversation.context_type = "application".to_string();
        conversation.title = Some("test-nextjs".to_string());
        let mut live = svc.subscribe_conversation(conversation.id);

        let stream = svc
            .try_tool_loop_in_workspace(
                &conversation,
                vec![],
                None,
                vec![],
                &test_auth(),
                &test_request_metadata(),
                None,
                None,
                temps_ai::SensitiveEnvironment::default(),
                None,
                true,
                None,
            )
            .await;
        drain(stream).await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let event = live.recv().await.expect("live event");
                if event.event == "conversation_title" {
                    break event;
                }
            }
        })
        .await
        .expect("title event timeout");
        assert_eq!(event.event, "conversation_title");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&event.data).expect("title JSON")["title"],
            "Create MongoDB Instance"
        );
    }

    // (a) a round calls a tool, the next round answers in prose -> the tool is
    // executed (ToolCall -> ToolResult, live) and the prose streams as the answer.
    #[tokio::test]
    async fn test_tool_loop_executes_tool_then_returns_prose() {
        let ai = Arc::new(ScriptedAi::new(vec![
            // Round 1: the model streams a tool call.
            Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                id: "c1".to_string(),
                name: "echo".to_string(),
                arguments: "{}".to_string(),
            })]),
            // Round 2: the model answers in prose (streamed in two deltas).
            Ok(vec![
                ChatStreamDelta::Text("final ".to_string()),
                ChatStreamDelta::Text("answer".to_string()),
            ]),
        ]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let tool_count = provider.tool_calls.clone();
        let chat_count = ai.chat_calls.clone();
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;
        let out = drain(stream).await;

        // The tool call surfaces as ToolCall -> ToolResult (live), then the final
        // prose streams token-by-token.
        assert_eq!(
            out,
            vec![
                ChatStreamEvent::ToolCall {
                    id: "c1".to_string(),
                    name: "echo".to_string(),
                    arguments: "{}".to_string(),
                },
                ChatStreamEvent::ToolResult {
                    id: "c1".to_string(),
                    name: "echo".to_string(),
                    content:
                        "Tool completed; detailed result is withheld from the chat transcript."
                            .to_string(),
                },
                ChatStreamEvent::Token("final ".to_string()),
                ChatStreamEvent::Token("answer".to_string()),
            ]
        );
        assert_eq!(tool_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(chat_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    // (a2) a plain conversational turn streams multiple text deltas as separate
    // tokens from a single model call (true token streaming, no tools).
    #[tokio::test]
    async fn test_tool_loop_streams_plain_answer_token_by_token() {
        let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![
            ChatStreamDelta::Text("Hello, ".to_string()),
            ChatStreamDelta::Text("world".to_string()),
        ])]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let chat_count = ai.chat_calls.clone();
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;
        let out = drain(stream).await;

        assert_eq!(
            out,
            vec![
                ChatStreamEvent::Token("Hello, ".to_string()),
                ChatStreamEvent::Token("world".to_string()),
            ]
        );
        // One model call only — no separate gather pass.
        assert_eq!(chat_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    // (b) the model call errors on round 1 -> the loop ends with no output.
    #[tokio::test]
    async fn test_tool_loop_call_error_yields_no_output() {
        let ai = Arc::new(ScriptedAi::new(vec![Err(AiError::Provider {
            purpose: "chat.test.tools".to_string(),
            reason: "boom".to_string(),
        })]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;
        let out = drain(stream).await;
        assert!(
            out.is_empty(),
            "errored call with no tools -> nothing; got {out:?}"
        );
    }

    #[tokio::test]
    async fn test_tool_loop_surfaces_provider_error_received_inside_stream() {
        let ai: Arc<dyn AiService> = Arc::new(StreamErrorAi);
        let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let (svc, tools) = service_with(ai);

        let mut stream = svc
            .try_tool_loop(
                &test_conversation(),
                vec![],
                Some(provider),
                tools,
                &test_auth(),
            )
            .await;
        let error = stream
            .next()
            .await
            .expect("the empty turn should report its provider failure")
            .expect_err("the event should be an error");

        assert!(
            error.to_string().contains("Token refresh failed: 401"),
            "the concrete streamed provider error must reach the user: {error}"
        );
        assert!(
            !error.to_string().contains("Check the provider's key and model"),
            "sandbox and harness failures must not be mislabeled as API key/model failures: {error}"
        );
    }

    // (c) A model stuck repeating itself is stopped by the unproductive-round
    // detector, long before the runaway backstop — and the user is told why.
    #[tokio::test]
    async fn test_tool_loop_stops_a_stuck_model_quickly() {
        // Empty script: the exhausted fallback always re-issues the SAME tool
        // call, so every round after the first is answered by the repeat guard
        // and produces nothing usable. That is the definition of stuck.
        let ai = Arc::new(ScriptedAi::new(vec![]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let chat_count = ai.chat_calls.clone();
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;
        let out = drain(stream).await;

        // Stopped in a handful of rounds, NOT at the 40-round backstop. The
        // backstop exists for a loop nobody is watching; a user staring at a
        // model repeating itself should not wait that long.
        let calls = chat_count.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            calls <= 6,
            "a stuck model must be stopped quickly, took {calls} rounds"
        );

        // And it must say so. A turn that halts without explanation is
        // indistinguishable from one that simply gave up.
        assert!(
            joined_text(&out).contains("produced only errors"),
            "the user must be told why it stopped; got {out:?}"
        );
    }

    /// A model making genuine progress is NOT cut off by a step count.
    ///
    /// This is the property the product wants: ask the chat something and it
    /// works the problem, rather than stopping every N turns. The old 10-step
    /// cap ended real alert-suggestion turns with nothing proposed; steps no
    /// longer govern at all.
    #[tokio::test]
    async fn test_tool_loop_does_not_cut_off_a_productive_model() {
        // 60 rounds, each a DIFFERENT call, so the repeat guard never fires and
        // every round counts as progress. Comfortably past any old step cap.
        let rounds: Vec<Result<Vec<ChatStreamDelta>, AiError>> = (0..60)
            .map(|i| {
                Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                    id: format!("c{i}"),
                    name: "echo".to_string(),
                    arguments: format!("{{\"n\":{i}}}"),
                })])
            })
            .collect();
        let ai = Arc::new(ScriptedAi::new(rounds));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let chat_count = ai.chat_calls.clone();
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;
        drain(stream).await;

        let calls = chat_count.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            calls >= 60,
            "a productive model must run its course, was stopped after {calls} rounds"
        );
    }

    /// Time is the limit. An unattended turn ends on the deadline however many
    /// steps it has taken, and says so.
    #[tokio::test(start_paused = true)]
    async fn test_tool_loop_ends_on_the_deadline() {
        // Never-ending script; each call advances the paused clock by a minute,
        // so the 15-minute deadline trips without the test waiting.
        let ai = Arc::new(
            ScriptedAi::new(
                (0..1000)
                    .map(|i| {
                        Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                            id: format!("c{i}"),
                            name: "echo".to_string(),
                            arguments: format!("{{\"n\":{i}}}"),
                        })])
                    })
                    .collect(),
            )
            .advancing(std::time::Duration::from_secs(60)),
        );
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let chat_count = ai.chat_calls.clone();
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;
        let out = drain(stream).await;

        let calls = chat_count.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            (14..=18).contains(&calls),
            "should stop around the 15-minute mark, stopped after {calls} rounds"
        );
        assert!(
            joined_text(&out).contains("time limit for a single turn"),
            "the user must be told time ran out; got {out:?}"
        );
    }

    // (c2) Salvage: after the loop exhausts MAX_ROUNDS still wanting tools, one
    // final tool-free call lets the model answer from the gathered evidence.
    #[tokio::test]
    async fn test_tool_loop_salvages_evidence_after_round_cap() {
        // MAX_ROUNDS tool-call rounds (never settles on prose), then the tool-free
        // salvage call finally answers. Each round uses a DISTINCT argument so the
        // anti-repeat dedup guard doesn't short-circuit it — we're exercising the
        // round cap here, not the repeat guard.
        let mut rounds: Vec<Result<Vec<ChatStreamDelta>, AiError>> = (0..10)
            .map(|i| {
                Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                    id: format!("c{i}"),
                    name: "echo".to_string(),
                    arguments: format!("{{\"n\":{i}}}"),
                })])
            })
            .collect();
        rounds.push(Ok(vec![ChatStreamDelta::Text(
            "salvaged answer".to_string(),
        )]));
        let ai = Arc::new(ScriptedAi::new(rounds));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let chat_count = ai.chat_calls.clone();
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;
        let out = drain(stream).await;

        assert_eq!(
            joined_text(&out),
            "salvaged answer",
            "the salvage call's prose should be streamed; got {out:?}"
        );
        assert_eq!(
            chat_count.load(std::sync::atomic::Ordering::SeqCst),
            11,
            "10 tool rounds + 1 salvage call"
        );
    }

    // (d) a round that streams only empty text -> no answer text is produced.
    #[tokio::test]
    async fn test_tool_loop_empty_final_text_yields_no_text() {
        let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![ChatStreamDelta::Text(
            String::new(),
        )])]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;
        let out = drain(stream).await;
        assert!(joined_text(&out).is_empty());
    }

    #[tokio::test]
    async fn temps_tool_uses_the_current_turns_auth_context() {
        let seen_user_id = Arc::new(std::sync::atomic::AtomicI32::new(-1));
        let provider: Arc<dyn ConversationContextProvider> = Arc::new(AuthRecordingProvider {
            seen_user_id: seen_user_id.clone(),
        });
        let auth = test_auth();
        let mut execution = ToolExecutionState::default();
        let result = dispatch_conversation_tool(
            &ToolCall {
                id: "auth-call".to_string(),
                name: "temps".to_string(),
                arguments: r#"{"command":"projects get_projects"}"#.to_string(),
            },
            Some(7),
            1,
            "42",
            &auth,
            None,
            None,
            Some(&provider),
            None,
            None,
            None,
            None,
            None,
            &mut execution,
        )
        .await;

        assert_eq!(result, "authenticated result");
        assert_eq!(
            seen_user_id.load(std::sync::atomic::Ordering::SeqCst),
            auth.user_id(),
            "API tools must receive the request user's AuthContext, never server or creator auth"
        );
    }

    // --- service-layer DB tests (MockDatabase) ------------------------------

    /// A `ConversationService` backed by the given mock DB. The AI is a dummy
    /// (`ScriptedAi` with no scripted responses) since these tests exercise only
    /// the DB query/scoping logic, never an AI turn.
    fn db_service(db: DatabaseConnection) -> ConversationService {
        db_service_from_arc(Arc::new(db))
    }

    fn db_service_from_arc(db: Arc<DatabaseConnection>) -> ConversationService {
        ConversationService {
            db,
            ai: Arc::new(ScriptedAi::new(vec![])),
            providers: HashMap::new(),
            write_support: None,
            config: None,
            application_workspaces: None,
            application_sandboxes: None,
            application_service: None,
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
            harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    fn db_service_with_ai(db: DatabaseConnection, ai: Arc<dyn AiService>) -> ConversationService {
        ConversationService {
            db: Arc::new(db),
            ai,
            providers: HashMap::new(),
            write_support: None,
            config: None,
            application_workspaces: None,
            application_sandboxes: None,
            application_service: None,
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
            harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    fn stored_message(id: i64, conversation_id: i64, role: &str) -> ai_messages::Model {
        ai_messages::Model {
            id,
            conversation_id,
            role: role.to_string(),
            content: format!("{role}-{id}"),
            metadata: None,
            tokens_in: None,
            tokens_out: None,
            cost_microcents: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn messages_page_returns_latest_page_oldest_first_with_cursor() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![
                    stored_message(8, 42, "assistant"),
                    stored_message(7, 42, "user"),
                    stored_message(6, 42, "assistant"),
                ]])
                .into_connection(),
        );
        let service = db_service_from_arc(db.clone());

        let page = service
            .messages_page(42, None, 2)
            .await
            .expect("latest message page");

        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![7, 8]
        );
        assert!(page.has_more);
        assert_eq!(page.next_before.as_deref(), Some("m1_7"));
        assert_eq!(
            decode_message_before_cursor(page.next_before.as_deref().expect("page cursor")),
            Ok(7)
        );

        drop(service);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let transaction_log = db.into_transaction_log();
        let statement = &transaction_log[0].statements()[0];
        assert!(statement
            .sql
            .contains("ORDER BY \"ai_messages\".\"id\" DESC"));
        assert!(statement.sql.contains("LIMIT"));
    }

    #[tokio::test]
    async fn messages_page_before_cursor_is_exclusive_and_pages_earlier_rows() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![
                    stored_message(6, 42, "assistant"),
                    stored_message(5, 42, "user"),
                    stored_message(4, 42, "assistant"),
                ]])
                .into_connection(),
        );
        let service = db_service_from_arc(db.clone());

        let page = service
            .messages_page(42, Some(7), 2)
            .await
            .expect("earlier message page");

        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![5, 6]
        );
        assert!(page.has_more);
        assert_eq!(page.next_before.as_deref(), Some("m1_5"));

        drop(service);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let transaction_log = db.into_transaction_log();
        let statement = &transaction_log[0].statements()[0];
        assert!(statement.sql.contains("\"ai_messages\".\"id\" <"));
        assert!(format!("{statement:?}").contains('7'));
    }

    #[tokio::test]
    async fn messages_page_omits_cursor_when_no_earlier_rows_remain() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                stored_message(2, 42, "assistant"),
                stored_message(1, 42, "user"),
            ]])
            .into_connection();

        let page = db_service(db)
            .messages_page(42, None, 2)
            .await
            .expect("complete message page");

        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(!page.has_more);
        assert_eq!(page.next_before, None);
    }

    #[tokio::test]
    async fn messages_page_excludes_internal_system_and_summary_rows() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![
                    stored_message(4, 42, "summary"),
                    stored_message(3, 42, "assistant"),
                    stored_message(2, 42, "system"),
                    stored_message(1, 42, "user"),
                ]])
                .into_connection(),
        );
        let service = db_service_from_arc(db.clone());

        let page = service
            .messages_page(42, None, 10)
            .await
            .expect("visible message page");

        assert_eq!(
            page.messages
                .iter()
                .map(|message| (message.id, message.role.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "user"), (3, "assistant")]
        );

        drop(service);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let statement = format!("{:?}", db.into_transaction_log()[0].statements()[0]);
        assert!(statement.contains("system"));
        assert!(statement.contains("summary"));
    }

    #[tokio::test]
    async fn provider_aware_availability_accepts_authenticated_host_harness() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service_with_ai(db, Arc::new(HostHarnessOnlyAi));

        assert!(
            !svc.ai_available().await,
            "the default gateway is unavailable"
        );
        assert!(
            svc.ai_available_for(Some("claude_cli")).await,
            "the selected host harness remains available"
        );
        assert!(!svc.ai_available_for(Some("gateway_key:1")).await);
    }

    #[tokio::test]
    async fn claim_turn_persists_one_server_owned_running_turn() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let service = db_service(db);
        let conversation = test_conversation();

        service
            .claim_turn(&conversation, "turn-one")
            .await
            .expect("the idle conversation should be claimed");
    }

    #[tokio::test]
    async fn claim_turn_rejects_an_idempotent_duplicate_without_appending_a_message() {
        let mut running = test_conversation();
        running.turn_status = "running".to_string();
        running.active_turn_id = Some("turn-one".to_string());
        running.last_turn_id = Some("turn-one".to_string());
        running.turn_started_at = Some(Utc::now());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([[running.clone()]])
            .into_connection();

        let error = db_service(db)
            .claim_turn(&running, "turn-one")
            .await
            .expect_err("a retry must not start a second harness turn");

        assert!(matches!(
            error,
            ChatError::DuplicateTurn { turn_id, .. } if turn_id == "turn-one"
        ));
    }

    #[tokio::test]
    async fn cancel_turn_clears_a_durable_claim_before_the_task_is_registered() {
        let mut running = test_conversation();
        running.turn_status = "running".to_string();
        running.active_turn_id = Some("turn-raced-stop".to_string());
        running.last_turn_id = running.active_turn_id.clone();
        running.turn_started_at = Some(Utc::now());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let service = db_service(db);
        let mut events = service.subscribe_conversation(running.id);

        assert!(service
            .cancel_turn(&running)
            .await
            .expect("the persisted claim should be cancellable without an abort handle"));
        let event = events
            .try_recv()
            .expect("observers should receive the terminal state");
        assert_eq!(event.event, "turn_complete");
    }

    #[tokio::test]
    async fn cancelled_claim_never_starts_provider_execution_after_registration() {
        let mut cancelled = test_conversation();
        cancelled.turn_status = "cancelled".to_string();
        cancelled.active_turn_id = None;
        cancelled.last_turn_id = Some("turn-cancelled-during-setup".to_string());
        cancelled.turn_started_at = None;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[cancelled]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![ChatStreamDelta::Text(
            "must not run".to_string(),
        )])]));
        let calls = ai.chat_calls.clone();
        let service = db_service_with_ai(db, ai);
        let conversation = test_conversation();

        let stream = service
            .try_tool_loop_in_workspace(
                &conversation,
                vec![],
                None,
                vec![],
                &test_auth(),
                &test_request_metadata(),
                None,
                None,
                temps_ai::SensitiveEnvironment::default(),
                Some("turn-cancelled-during-setup".to_string()),
                false,
                None,
            )
            .await;
        let output = drain(stream).await;

        assert!(output.is_empty());
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a task whose durable claim was cancelled must not reach the provider"
        );
    }

    #[tokio::test]
    async fn project_chat_preserves_reasoning_supported_by_responses_api() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = db_service(db);

        let runtime = service
            .resolve_conversation_runtime(
                Some("gateway_key:1"),
                Some("gpt-5.6-luna"),
                Some("medium"),
                Some("confirm-actions"),
            )
            .await
            .expect("Responses API supports reasoning with function tools");

        assert_eq!(runtime.thinking_level.as_deref(), Some("medium"));
    }

    #[tokio::test]
    async fn update_runtime_options_changes_model_without_switching_provider_and_resets_session() {
        let mut conversation = test_conversation();
        conversation.ai_provider = "codex_cli".to_string();
        conversation.cli_session_id = Some("session-1".to_string());
        conversation.cli_session_fingerprint = Some("v1:fingerprint".to_string());

        let mut updated = conversation.clone();
        updated.ai_model = "gpt-4.1".to_string();
        updated.ai_thinking_level = Some("low".to_string());
        updated.ai_permission_mode = "full-access".to_string();
        updated.cli_session_id = None;
        updated.cli_session_fingerprint = None;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[updated]])
            .into_connection();

        let result = db_service(db)
            .update_runtime_options(
                &conversation,
                Some("gpt-4.1"),
                Some("low"),
                Some("full-access"),
            )
            .await
            .expect("valid runtime options should persist");

        assert_eq!(result.ai_provider, "codex_cli");
        assert_eq!(result.ai_model, "gpt-4.1");
        assert_eq!(result.ai_thinking_level.as_deref(), Some("low"));
        assert_eq!(result.ai_permission_mode, "full-access");
        assert_eq!(result.cli_session_id, None);
        assert_eq!(result.cli_session_fingerprint, None);
    }

    #[tokio::test]
    async fn update_runtime_options_retains_session_when_only_thinking_and_permission_change() {
        let mut conversation = test_conversation();
        conversation.ai_provider = "codex_cli".to_string();
        conversation.cli_session_id = Some("session-1".to_string());
        conversation.cli_session_fingerprint = Some("v1:fingerprint".to_string());

        let mut updated = conversation.clone();
        updated.ai_thinking_level = Some("high".to_string());
        updated.ai_permission_mode = "full-access".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[updated]])
            .into_connection();

        let result = db_service(db)
            .update_runtime_options(&conversation, None, Some("high"), Some("full-access"))
            .await
            .expect("turn-level options should persist");

        assert_eq!(result.ai_provider, "codex_cli");
        assert_eq!(result.ai_model, conversation.ai_model);
        assert_eq!(result.cli_session_id.as_deref(), Some("session-1"));
        assert_eq!(
            result.cli_session_fingerprint.as_deref(),
            Some("v1:fingerprint")
        );
    }

    #[tokio::test]
    async fn update_runtime_options_rejects_options_outside_pinned_provider_capabilities() {
        let mut conversation = test_conversation();
        conversation.ai_provider = "codex_cli".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let error = db_service(db)
            .update_runtime_options(&conversation, Some("claude-opus"), None, None)
            .await
            .expect_err("a model outside the pinned harness must fail closed");

        assert!(matches!(error, ChatError::Ai(message) if
            message.contains("claude-opus") && message.contains("codex_cli")));
    }

    #[tokio::test]
    async fn update_runtime_options_preserves_database_failure() {
        let mut conversation = test_conversation();
        conversation.ai_provider = "codex_cli".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("write failed".to_string())])
            .into_connection();

        let error = db_service(db)
            .update_runtime_options(&conversation, None, Some("high"), None)
            .await
            .expect_err("database failures must not look like successful updates");

        assert!(
            matches!(error, ChatError::Db(sea_orm::DbErr::Custom(message)) if
            message == "write failed")
        );
    }

    /// Build a conversation row for a given project, with controllable public_id.
    fn conv_for(id: i64, project_id: i32, public_id: &str) -> ai_conversations::Model {
        let now = Utc::now();
        ai_conversations::Model {
            id,
            public_id: public_id.to_string(),
            project_id: Some(project_id),
            application_id: None,
            context_type: "deployment".to_string(),
            context_id: "1".to_string(),
            title: Some("t".to_string()),
            status: "active".to_string(),
            created_by: 5,
            metadata: None,
            cli_session_id: None,
            cli_session_fingerprint: None,
            ai_provider: "gateway".to_string(),
            ai_model: "gpt-4o-mini".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "confirm-actions".to_string(),
            turn_status: "idle".to_string(),
            active_turn_id: None,
            last_turn_id: None,
            turn_started_at: None,
            created_at: now,
            last_activity_at: now,
        }
    }

    fn system_message(id: i64, conversation_id: i64) -> ai_messages::Model {
        ai_messages::Model {
            id,
            conversation_id,
            role: "system".to_string(),
            content: "private system context".to_string(),
            metadata: None,
            tokens_in: None,
            tokens_out: None,
            cost_microcents: None,
            created_at: Utc::now(),
        }
    }

    /// Build a minimal valid legacy project row. Toggle values are varied to
    /// prove they no longer influence current chat authorization.
    fn project_with_toggle(
        id: i32,
        name: &str,
        slug: &str,
        toggle: Option<bool>,
    ) -> temps_entities::projects::Model {
        let now = Utc::now();
        temps_entities::projects::Model {
            id,
            image_retention_hours: None,
            name: name.to_string(),
            repo_name: "r".to_string(),
            repo_owner: "o".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::Static,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: slug.to_string(),
            template_slug: None,
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_api_traffic_summary_enabled: None,
            allow_alternate_sources: None,
            ai_debug_chat_enabled: toggle,
            ai_write_actions_enabled: false,
            error_source_context_enabled: false,
            vulnerability_scanning_enabled: false,
            error_source_root: None,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
            project_type: temps_entities::types::ProjectType::Server,
            service_template: None,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            cross_project_trace_sharing: true,
        }
    }

    #[tokio::test]
    async fn two_users_get_separate_conversations_for_the_same_context() {
        let mut user_11 = conv_for(1, 7, "user11");
        user_11.created_by = 11;
        user_11.ai_provider = "gateway_key:1".to_string();
        let mut user_22 = conv_for(2, 7, "user22");
        user_22.created_by = 22;
        user_22.ai_provider = "gateway_key:1".to_string();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .append_query_results([vec![user_11.clone()]])
            .append_query_results([vec![system_message(1, user_11.id)]])
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .append_query_results([vec![user_22.clone()]])
            .append_query_results([vec![system_message(2, user_22.id)]])
            .into_connection();
        let mut providers: HashMap<&'static str, Arc<dyn ConversationContextProvider>> =
            HashMap::new();
        providers.insert("deployment", Arc::new(SeedOnlyProvider));
        let svc = ConversationService {
            db: Arc::new(db),
            ai: Arc::new(ScriptedAi::new(vec![])),
            providers,
            write_support: None,
            config: None,
            application_workspaces: None,
            application_sandboxes: None,
            application_service: None,
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
            harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
        };

        let first = svc
            .get_or_create(
                Some(7),
                "deployment",
                "1",
                11,
                Some("gateway_key:1"),
                Some("gpt-4o-mini"),
                None,
                Some("confirm-actions"),
            )
            .await
            .expect("first user's private conversation");
        let second = svc
            .get_or_create(
                Some(7),
                "deployment",
                "1",
                22,
                Some("gateway_key:1"),
                Some("gpt-4o-mini"),
                None,
                Some("confirm-actions"),
            )
            .await
            .expect("second user's private conversation");

        assert_eq!(first.created_by, 11);
        assert_eq!(second.created_by, 22);
        assert_ne!(first.public_id, second.public_id);
    }

    #[tokio::test]
    async fn chat_readiness_ignores_legacy_project_chat_opt_out() {
        let project = project_with_toggle(7, "Alpha", "alpha", Some(false));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![project]])
            .into_connection();

        let readiness = db_service(db).chat_readiness(7).await.expect("readiness");
        assert_eq!(
            readiness,
            ChatReadiness {
                ai_configured: true,
            }
        );
    }

    #[tokio::test]
    async fn chat_readiness_reports_unconfigured_ai() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![project_with_toggle(
                7,
                "Alpha",
                "alpha",
                Some(true),
            )]])
            .into_connection();
        let svc = db_service_with_ai(db, Arc::new(ScriptedAi::unavailable()));

        let readiness = svc.chat_readiness(7).await.expect("readiness");
        assert!(!readiness.ai_configured);
    }

    #[tokio::test]
    async fn chat_readiness_returns_typed_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::projects::Model>::new()])
            .into_connection();

        let err = db_service(db)
            .chat_readiness(404)
            .await
            .expect_err("missing project");
        assert!(matches!(err, ChatError::ProjectNotFound(404)));
    }

    #[tokio::test]
    async fn chat_readiness_preserves_project_context_on_database_failure() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection lost".to_string())])
            .into_connection();

        let err = db_service(db)
            .chat_readiness(42)
            .await
            .expect_err("query failure");
        assert!(matches!(
            err,
            ChatError::ProjectLookup { project_id: 42, .. }
        ));
    }

    // find_by_context: returns the active conversation when one exists.
    #[tokio::test]
    async fn test_find_by_context_returns_match() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
            .into_connection();
        let svc = db_service(db);

        let found = svc
            .find_by_context(Some(7), 5, "deployment", "1")
            .await
            .expect("query ok");
        let conv = found.expect("a conversation should be found");
        assert_eq!(conv.project_id, Some(7));
        assert_eq!(conv.public_id, "pubA");
    }

    // find_by_context: returns None when no row matches.
    #[tokio::test]
    async fn test_find_by_context_none_when_absent() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_conversations::Model>::new()])
            .into_connection();
        let svc = db_service(db);

        let found = svc
            .find_by_context(Some(7), 5, "deployment", "1")
            .await
            .expect("query ok");
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn every_conversation_lookup_is_scoped_to_the_current_creator() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<ai_conversations::Model>::new()])
                .append_query_results([Vec::<ai_conversations::Model>::new()])
                .append_query_results([Vec::<ai_conversations::Model>::new()])
                .append_query_results([Vec::<ai_conversations::Model>::new()])
                .append_query_results([Vec::<ai_conversations::Model>::new()])
                .into_connection(),
        );
        let svc = db_service_from_arc(db.clone());

        assert!(svc
            .find_by_context(Some(7), 11, "deployment", "1")
            .await
            .expect("context lookup")
            .is_none());
        assert!(svc
            .list_conversations(7, 11)
            .await
            .expect("project list")
            .is_empty());
        assert!(svc
            .list_all_conversations(11, &[])
            .await
            .expect("global list")
            .is_empty());
        assert!(matches!(
            svc.get_by_public_id(7, 11, "owned-session").await,
            Err(ChatError::NotFound(_))
        ));
        assert!(matches!(
            svc.get_by_id(7, 11, 99).await,
            Err(ChatError::NotFound(_))
        ));

        drop(svc);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let log = db.into_transaction_log();
        let statements = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .collect::<Vec<_>>();
        assert_eq!(statements.len(), 5);
        assert!(
            statements.iter().all(|statement| {
                statement.sql.contains("created_by") && format!("{statement:?}").contains("11")
            }),
            "every query must bind created_by = current user; got {statements:?}"
        );
    }

    #[tokio::test]
    async fn another_project_member_cannot_load_or_resume_an_owned_cli_session() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<ai_conversations::Model>::new()])
                .into_connection(),
        );
        let svc = db_service_from_arc(db.clone());

        let error = svc
            .get_by_public_id(7, 22, "user-11-claude-session")
            .await
            .expect_err("another member must receive the same not-found as an unknown id");
        assert!(matches!(error, ChatError::NotFound(id) if id == "user-11-claude-session"));

        drop(svc);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let dump = format!("{:?}", db.into_transaction_log());
        assert!(dump.contains("created_by") && dump.contains("22"));
    }

    #[tokio::test]
    async fn test_get_by_id_retains_project_scope() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
            .into_connection();
        let conv = db_service(db)
            .get_by_id(7, 5, 1)
            .await
            .expect("conversation");
        assert_eq!(conv.public_id, "pubA");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_conversations::Model>::new()])
            .into_connection();
        let err = db_service(db)
            .get_by_id(8, 5, 1)
            .await
            .expect_err("cross-project child lookup must not resolve");
        assert!(matches!(err, ChatError::NotFound(_)));
    }

    // list_conversations: returns the project's active conversations.
    #[tokio::test]
    async fn test_list_conversations_returns_rows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![conv_for(1, 7, "pubA"), conv_for(2, 7, "pubB")]])
            .into_connection();
        let svc = db_service(db);

        let convs = svc.list_conversations(7, 5).await.expect("query ok");
        assert_eq!(convs.len(), 2);
        assert!(convs.iter().all(|c| c.project_id == Some(7)));
    }

    // list_all_conversations: annotates each conversation with its project's
    // name/slug, scoped to the current creator.
    #[tokio::test]
    async fn test_list_all_conversations_annotates_enabled_projects() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1st query: the conversations.
            .append_query_results(vec![vec![conv_for(1, 7, "pubA"), conv_for(2, 8, "pubB")]])
            // 2nd query: the projects for those ids (both enabled).
            .append_query_results(vec![vec![
                project_with_toggle(7, "Alpha", "alpha", Some(true)),
                project_with_toggle(8, "Beta", "beta", Some(true)),
            ]])
            .into_connection();
        let svc = db_service(db);

        let items = svc.list_all_conversations(5, &[]).await.expect("query ok");
        assert_eq!(items.len(), 2);
        let alpha = items
            .iter()
            .find(|i| i.conversation.project_id == Some(7))
            .expect("alpha present");
        assert_eq!(alpha.project_name.as_deref(), Some("Alpha"));
        assert_eq!(alpha.project_slug.as_deref(), Some("alpha"));
    }

    #[tokio::test]
    async fn test_list_all_conversations_excludes_currently_hidden_projects() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![conv_for(1, 7, "hidden"), conv_for(2, 8, "visible")]])
            .append_query_results([vec![project_with_toggle(
                8,
                "Visible",
                "visible",
                Some(true),
            )]])
            .into_connection();

        let items = db_service(db)
            .list_all_conversations(5, &[7])
            .await
            .expect("hidden projects filter");

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].conversation.public_id, "visible");
    }

    // Legacy project toggle values no longer affect chat visibility. Access is
    // determined by the current user's project membership and permissions.
    #[tokio::test]
    async fn test_list_all_conversations_ignores_legacy_project_toggle() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                conv_for(1, 7, "pubEnabled"),
                conv_for(2, 8, "pubDisabled"),
                conv_for(3, 9, "pubNull"),
            ]])
            .append_query_results(vec![vec![
                project_with_toggle(7, "Alpha", "alpha", Some(true)),
                project_with_toggle(8, "Beta", "beta", Some(false)),
                project_with_toggle(9, "Gamma", "gamma", None),
            ]])
            .into_connection();
        let svc = db_service(db);

        let items = svc.list_all_conversations(5, &[]).await.expect("query ok");
        assert_eq!(
            items.len(),
            3,
            "all accessible project conversations stay visible"
        );
        assert!(items
            .iter()
            .any(|i| i.conversation.public_id == "pubDisabled"));
        assert!(items
            .iter()
            .any(|i| i.conversation.public_id == "pubEnabled"));
        assert!(items.iter().any(|i| i.conversation.public_id == "pubNull"));
    }

    // list_all_conversations: also excludes conversations whose project row is
    // missing entirely (defensive — a dangling project_id must not leak).
    #[tokio::test]
    async fn test_list_all_conversations_excludes_missing_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
            // Project lookup returns nothing for id 7.
            .append_query_results(vec![Vec::<temps_entities::projects::Model>::new()])
            .into_connection();
        let svc = db_service(db);

        let items = svc.list_all_conversations(5, &[]).await.expect("query ok");
        assert!(items.is_empty());
    }

    // get_by_public_id: returns the row when the (project_id, public_id) pair
    // matches; the filter scopes to the project so a wrong project can't fetch it.
    #[tokio::test]
    async fn test_get_by_public_id_returns_scoped_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
            .into_connection();
        let svc = db_service(db);

        let conv = svc.get_by_public_id(7, 5, "pubA").await.expect("found");
        assert_eq!(conv.project_id, Some(7));
        assert_eq!(conv.public_id, "pubA");
    }

    // get_by_public_id: when the scoped query returns no row (e.g. wrong project
    // or unknown id), a `NotFound` carrying the public_id is returned.
    #[tokio::test]
    async fn test_get_by_public_id_not_found_is_scoped_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_conversations::Model>::new()])
            .into_connection();
        let svc = db_service(db);

        let err = svc
            .get_by_public_id(99, 5, "pubA")
            .await
            .expect_err("should not find a conversation in the wrong project");
        match err {
            ChatError::NotFound(id) => assert_eq!(id, "pubA"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // archive: flips status to "archived" via an UPDATE returning the row.
    #[tokio::test]
    async fn test_archive_succeeds() {
        let mut archived = conv_for(1, 7, "pubA");
        archived.status = "archived".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![archived]])
            .into_connection();
        let svc = db_service(db);

        let conv = conv_for(1, 7, "pubA");
        svc.archive(&conv).await.expect("archive ok");
    }

    #[tokio::test]
    async fn test_archive_rejects_a_running_conversation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);
        let mut running = conv_for(1, 7, "pubA");
        running.turn_status = "running".to_string();

        let error = svc
            .archive(&running)
            .await
            .expect_err("a running conversation must remain visible and cancellable");

        assert!(matches!(
            error,
            ChatError::TurnInProgress { conversation_id } if conversation_id == "pubA"
        ));
    }

    #[tokio::test]
    async fn test_restore_succeeds() {
        let active = conv_for(1, 7, "pubA");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![active]])
            .into_connection();
        let svc = db_service(db);

        let mut archived = conv_for(1, 7, "pubA");
        archived.status = "archived".to_string();
        svc.restore(&archived).await.expect("restore ok");
    }

    // ----- Pending-permission registry tests (ADR-038 Phase 2, milestone 3) -----

    /// A freshly constructed `ConversationService` has an empty registry.
    #[tokio::test]
    async fn test_registry_starts_empty() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);
        let registry = svc.pending_permissions.lock().unwrap();
        assert!(registry.is_empty());
    }

    /// Insert and immediately resolve: sender receives the decision.
    #[tokio::test]
    async fn test_registry_insert_and_resolve() {
        use temps_ai::streaming::{PermissionDecision, PermissionKind};
        use tokio::sync::oneshot;

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);

        let (tx, rx) = oneshot::channel::<PermissionResolution>();
        {
            let mut registry = svc.pending_permissions.lock().unwrap();
            registry.insert(
                "req-1".to_string(),
                PendingPermissionEntry {
                    sender: tx,
                    conv_public_id: "pub1".to_string(),
                    kind: PermissionKind::ToolApproval,
                    tool_name: "Bash".to_string(),
                    input: serde_json::Value::Null,
                    generation: uuid::Uuid::new_v4(),
                    origin: PendingPermissionOrigin::Provider,
                },
            );
        }

        // Resolve via remove-to-claim.
        let entry = {
            let mut registry = svc.pending_permissions.lock().unwrap();
            registry.remove("req-1")
        };

        assert!(entry.is_some(), "entry must be present after insert");
        let entry = entry.unwrap();
        assert_eq!(entry.conv_public_id, "pub1");
        assert_eq!(entry.kind, PermissionKind::ToolApproval);
        let sent = entry.sender.send(PermissionResolution {
            decision: PermissionDecision::AllowTool,
            auth: test_auth(),
            metadata: test_request_metadata(),
        });
        assert!(sent.is_ok(), "the waiting turn must receive the resolution");
        let resolution = rx.await.unwrap();
        assert!(matches!(resolution.decision, PermissionDecision::AllowTool));
    }

    #[tokio::test]
    async fn active_auto_mode_resolves_safe_tools_but_not_destructive_writes() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);
        let running = tokio::spawn(std::future::pending::<()>());
        let auto = Arc::new(AtomicBool::new(false));
        svc.active_turns.lock().unwrap().insert(
            7,
            ActiveTurn {
                turn_id: "turn-7".to_string(),
                abort: running.abort_handle(),
                auto_approve_provider_tools: auto.clone(),
            },
        );

        let (provider_tx, provider_rx) = oneshot::channel();
        let (platform_tx, platform_rx) = oneshot::channel();
        let (destructive_tx, _destructive_rx) = oneshot::channel();
        let (question_tx, _question_rx) = oneshot::channel();
        {
            let mut registry = svc.pending_permissions.lock().unwrap();
            for (id, sender, kind, origin) in [
                (
                    "provider-tool",
                    provider_tx,
                    PermissionKind::ToolApproval,
                    PendingPermissionOrigin::Provider,
                ),
                (
                    "platform-tool",
                    platform_tx,
                    PermissionKind::ToolApproval,
                    PendingPermissionOrigin::PlatformWrite,
                ),
                (
                    "destructive-platform-tool",
                    destructive_tx,
                    PermissionKind::ToolApproval,
                    PendingPermissionOrigin::PlatformWrite,
                ),
                (
                    "provider-question",
                    question_tx,
                    PermissionKind::Question,
                    PendingPermissionOrigin::Provider,
                ),
            ] {
                registry.insert(
                    id.to_string(),
                    PendingPermissionEntry {
                        sender,
                        conv_public_id: "conversation-7".to_string(),
                        kind,
                        tool_name: "test".to_string(),
                        input: if id == "destructive-platform-tool" {
                            serde_json::json!({"method": "DELETE"})
                        } else {
                            serde_json::Value::Null
                        },
                        generation: uuid::Uuid::new_v4(),
                        origin,
                    },
                );
            }
        }

        let update = svc.apply_active_permission_mode(
            7,
            "conversation-7",
            "full-access",
            &test_auth(),
            &test_request_metadata(),
        );

        assert!(update.applied_to_active_turn);
        assert_eq!(update.auto_approved.len(), 2);
        let mut approved_ids = update
            .auto_approved
            .iter()
            .map(|approval| approval.id.as_str())
            .collect::<Vec<_>>();
        approved_ids.sort_unstable();
        assert_eq!(approved_ids, ["platform-tool", "provider-tool"]);
        assert!(update
            .auto_approved
            .iter()
            .all(|approval| approval.delivered));
        assert!(auto.load(Ordering::Acquire));
        assert!(matches!(
            provider_rx.await.unwrap().decision,
            PermissionDecision::AllowTool
        ));
        assert!(matches!(
            platform_rx.await.unwrap().decision,
            PermissionDecision::AllowTool
        ));
        let registry = svc.pending_permissions.lock().unwrap();
        assert!(!registry.contains_key("platform-tool"));
        assert!(registry.contains_key("destructive-platform-tool"));
        assert!(registry.contains_key("provider-question"));
        drop(registry);
        running.abort();
    }

    #[test]
    fn platform_auto_approval_never_includes_delete_operations() {
        let safe = PermissionRequest {
            id: "safe".to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
            input: serde_json::json!({"method": "POST"}),
        };
        assert!(matches!(
            automatic_platform_decision(&safe),
            Some(PermissionDecision::AllowTool)
        ));

        let destructive = PermissionRequest {
            id: "delete".to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
            input: serde_json::json!({"method": "DELETE"}),
        };
        assert!(automatic_platform_decision(&destructive).is_none());

        let destructive_plan = PermissionRequest {
            id: "plan".to_string(),
            kind: PermissionKind::PlanApproval,
            tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
            input: serde_json::json!({
                "steps": [
                    {"method": "PATCH"},
                    {"method": "delete"}
                ]
            }),
        };
        assert!(automatic_platform_decision(&destructive_plan).is_none());
    }

    /// Double-resolve: the second remove returns None (409 semantic).
    #[tokio::test]
    async fn test_registry_double_resolve_returns_none() {
        use temps_ai::streaming::PermissionKind;
        use tokio::sync::oneshot;

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);

        let (tx, _rx) = oneshot::channel::<PermissionResolution>();
        svc.pending_permissions.lock().unwrap().insert(
            "req-dup".to_string(),
            PendingPermissionEntry {
                sender: tx,
                conv_public_id: "pub-dup".to_string(),
                kind: PermissionKind::ToolApproval,
                tool_name: "Bash".to_string(),
                input: serde_json::Value::Null,
                generation: uuid::Uuid::new_v4(),
                origin: PendingPermissionOrigin::Provider,
            },
        );

        // First claim succeeds.
        let first = svc.pending_permissions.lock().unwrap().remove("req-dup");
        assert!(first.is_some());
        drop(first); // entry (including sender) dropped

        // Second claim finds nothing (409 → 404 from the handler's perspective).
        let second = svc.pending_permissions.lock().unwrap().remove("req-dup");
        assert!(
            second.is_none(),
            "second remove must return None (already claimed)"
        );
    }

    /// Unknown id: remove returns None (404 semantic).
    #[tokio::test]
    async fn test_registry_unknown_id_returns_none() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);

        let result = svc.pending_permissions.lock().unwrap().remove("no-such-id");
        assert!(result.is_none(), "unknown id must return None");
    }

    #[test]
    fn cancelled_permission_guard_removes_only_its_registration() {
        use temps_ai::streaming::PermissionKind;
        use tokio::sync::oneshot;

        let registry = Arc::new(Mutex::new(HashMap::new()));
        let old_generation = uuid::Uuid::new_v4();
        let newer_generation = uuid::Uuid::new_v4();
        let (sender, _receiver) = oneshot::channel();
        registry.lock().unwrap().insert(
            "shared-id".to_string(),
            PendingPermissionEntry {
                sender,
                conv_public_id: "conversation".to_string(),
                kind: PermissionKind::ToolApproval,
                tool_name: "temps".to_string(),
                input: serde_json::Value::Null,
                generation: newer_generation,
                origin: PendingPermissionOrigin::Provider,
            },
        );

        drop(PendingPermissionGuard {
            registry: registry.clone(),
            permission_id: "shared-id".to_string(),
            generation: old_generation,
        });
        assert_eq!(
            registry.lock().unwrap()["shared-id"].generation,
            newer_generation,
            "an older cancelled waiter must not delete a replacement"
        );

        drop(PendingPermissionGuard {
            registry: registry.clone(),
            permission_id: "shared-id".to_string(),
            generation: newer_generation,
        });
        assert!(registry.lock().unwrap().is_empty());
    }

    /// Drain-on-close: when the subprocess exits (simulate by dropping the receiver),
    /// a synthetic deny sent to the still-pending sender propagates the "denied"
    /// decision and unblocks any waiting task.
    #[tokio::test]
    async fn test_registry_drain_on_close_denies_pending() {
        use temps_ai::streaming::{PermissionDecision, PermissionKind};
        use tokio::sync::oneshot;

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);

        let (tx, rx) = oneshot::channel::<PermissionResolution>();
        svc.pending_permissions.lock().unwrap().insert(
            "req-drain".to_string(),
            PendingPermissionEntry {
                sender: tx,
                conv_public_id: "pub-drain".to_string(),
                kind: PermissionKind::ToolApproval,
                tool_name: "Bash".to_string(),
                input: serde_json::Value::Null,
                generation: uuid::Uuid::new_v4(),
                origin: PendingPermissionOrigin::Provider,
            },
        );

        // Simulate subprocess exit: drain the registry with a synthetic deny.
        let drained: Vec<_> = {
            let mut registry = svc.pending_permissions.lock().unwrap();
            registry.drain().collect()
        };
        assert_eq!(drained.len(), 1, "one entry must have been drained");
        let (_id, entry) = drained.into_iter().next().unwrap();
        let deny_result = entry.sender.send(PermissionResolution {
            decision: PermissionDecision::DenyTool {
                reason: Some("subprocess exited".to_string()),
            },
            auth: test_auth(),
            metadata: test_request_metadata(),
        });
        assert!(deny_result.is_ok(), "send on drained entry must succeed");

        let resolution = rx.await.unwrap();
        assert!(
            matches!(resolution.decision, PermissionDecision::DenyTool { reason: Some(ref r) } if r.contains("exited")),
            "drained entry must deliver deny"
        );
    }

    /// A turn that produces nothing must SAY so.
    ///
    /// Before this, a provider failure just ended the round loop: the SSE stream
    /// closed with zero events, so the UI showed the user's own message and then
    /// nothing, forever — indistinguishable from a hang, and with no hint that
    /// the provider key was the problem. A self-hosted operator has no support
    /// channel to ask, so the error has to reach the screen.
    #[tokio::test]
    async fn empty_turn_emits_an_actionable_error_instead_of_silence() {
        // Every model call fails, including the tool-free salvage call.
        let ai = Arc::new(ScriptedAi::new(vec![
            Err(AiError::Provider {
                purpose: "chat.test.tools".to_string(),
                reason: "bad api key".to_string(),
            }),
            Err(AiError::Provider {
                purpose: "chat.test.tools.final".to_string(),
                reason: "bad api key".to_string(),
            }),
        ]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let mut stream = svc
            .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
            .await;

        let mut errors = Vec::new();
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(ev) => events.push(ev),
                Err(e) => errors.push(e.to_string()),
            }
        }

        assert!(
            events.is_empty(),
            "a failed turn should stream no content, got {events:?}"
        );
        assert_eq!(errors.len(), 1, "expected exactly one error event");
        let msg = &errors[0];
        assert!(
            msg.contains("bad api key"),
            "the underlying provider error must survive to the user: {msg}"
        );
        assert!(
            msg.contains("selected harness/provider status"),
            "the error must point at the selected runtime status: {msg}"
        );
    }

    #[test]
    fn live_wire_classifies_provider_failures_without_exposing_raw_diagnostics() {
        let event = wire_event_for(&Err(ChatError::Ai(
            "Invalid MCP configuration: EACCES open '/run/secrets/private.json' token=secret"
                .to_string(),
        )));
        let payload: serde_json::Value =
            serde_json::from_str(&event.data).expect("public failure is valid JSON");

        assert_eq!(event.event, "error");
        assert_eq!(payload["code"], "tool_configuration_unreadable");
        assert_eq!(payload["title"], "Application tools could not start");
        assert!(!event.data.contains("/run/secrets"));
        assert!(!event.data.contains("token=secret"));
    }

    #[tokio::test]
    async fn harness_mcp_capability_is_authenticated_scoped_and_removed_with_guard() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_executor = calls.clone();
        let executor: ToolExecutor = Arc::new(move |call| {
            let calls = calls_for_executor.clone();
            Box::pin(async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(format!("scoped:{}", call.arguments))
            })
        });
        let tools = vec![ChatTool {
            name: "temps".to_string(),
            description: "Scoped platform read".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let interactions: temps_ai::InteractionExecutor =
            Arc::new(|_| Box::pin(async { Ok(temps_ai::PermissionDecision::AllowTool) }));
        let (server, guard) = ConversationService::register_harness_mcp(
            svc.harness_mcp_entries.clone(),
            "http://host.docker.internal:8080",
            7,
            tools,
            executor,
            interactions,
            Duration::from_secs(60),
        );
        let bridge_id = server
            .url
            .split("/sandbox-tools/")
            .nth(1)
            .and_then(|suffix| suffix.split('/').next())
            .expect("bridge id in scoped URL");

        let unauthorized = svc
            .handle_harness_mcp_request(
                bridge_id,
                "wrong-token",
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            )
            .await;
        assert_eq!(unauthorized, Err(HarnessMcpError::Unauthorized));

        let response = svc
            .handle_harness_mcp_request(
                bridge_id,
                &server.authorization_token,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": "temps", "arguments": {"command": "--help"}}
                }),
            )
            .await
            .expect("authorized capability")
            .expect("request response");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(response["result"]["isError"], false);
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("--help")));

        drop(guard);
        let after_turn = svc
            .handle_harness_mcp_request(
                bridge_id,
                &server.authorization_token,
                serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
            )
            .await;
        assert_eq!(after_turn, Err(HarnessMcpError::NotFound));
    }

    #[tokio::test]
    async fn expired_harness_mcp_capability_fails_closed() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);
        let executor: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("unused".to_string()) }));
        let interactions: temps_ai::InteractionExecutor = Arc::new(|_| {
            Box::pin(async { Ok(temps_ai::PermissionDecision::DenyTool { reason: None }) })
        });
        let (server, _guard) = ConversationService::register_harness_mcp(
            svc.harness_mcp_entries.clone(),
            "http://host.docker.internal:8080",
            7,
            Vec::new(),
            executor,
            interactions,
            Duration::ZERO,
        );
        let bridge_id = server
            .url
            .split("/sandbox-tools/")
            .nth(1)
            .and_then(|suffix| suffix.split('/').next())
            .expect("bridge id in scoped URL");

        let result = svc
            .handle_harness_mcp_request(
                bridge_id,
                &server.authorization_token,
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            )
            .await;
        assert_eq!(result, Err(HarnessMcpError::Expired));
    }

    #[tokio::test]
    async fn harness_mcp_native_permission_round_trips_the_decision() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);
        let executor: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("unused".to_string()) }));
        let seen = Arc::new(std::sync::Mutex::new(None));
        let seen_for_interaction = seen.clone();
        let interactions: temps_ai::InteractionExecutor = Arc::new(move |request| {
            *seen_for_interaction.lock().expect("capture permission") = Some(request);
            Box::pin(async { Ok(temps_ai::PermissionDecision::AllowTool) })
        });
        let (server, _guard) = ConversationService::register_harness_mcp(
            svc.harness_mcp_entries.clone(),
            "http://host.docker.internal:8080",
            7,
            Vec::new(),
            executor,
            interactions,
            Duration::from_secs(60),
        );
        let bridge_id = server
            .url
            .split("/sandbox-tools/")
            .nth(1)
            .and_then(|suffix| suffix.split('/').next())
            .expect("bridge id in scoped URL");

        let response = svc
            .handle_harness_mcp_request(
                bridge_id,
                &server.authorization_token,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 9,
                    "method": "tools/call",
                    "params": {
                        "name": "temps_native_permission",
                        "arguments": {
                            "tool_name": "Bash",
                            "input": {"command": "npm install"}
                        }
                    }
                }),
            )
            .await
            .expect("authorized capability")
            .expect("request response");

        let permission = seen
            .lock()
            .expect("captured permission")
            .clone()
            .expect("permission was requested");
        assert_eq!(permission.tool_name, "Bash");
        assert_eq!(permission.input["command"], "npm install");
        let payload: serde_json::Value = serde_json::from_str(
            response["result"]["content"][0]["text"]
                .as_str()
                .expect("permission payload text"),
        )
        .expect("permission payload JSON");
        assert_eq!(payload["behavior"], "allow");
        assert_eq!(payload["updatedInput"]["command"], "npm install");
    }

    #[tokio::test]
    async fn harness_mcp_native_permission_returns_an_explicit_denial() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);
        let executor: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("unused".to_string()) }));
        let interactions: temps_ai::InteractionExecutor = Arc::new(|_| {
            Box::pin(async {
                Ok(temps_ai::PermissionDecision::DenyTool {
                    reason: Some("Denied in Temps".to_string()),
                })
            })
        });
        let (server, _guard) = ConversationService::register_harness_mcp(
            svc.harness_mcp_entries.clone(),
            "http://host.docker.internal:8080",
            7,
            Vec::new(),
            executor,
            interactions,
            Duration::from_secs(60),
        );
        let bridge_id = server
            .url
            .split("/sandbox-tools/")
            .nth(1)
            .and_then(|suffix| suffix.split('/').next())
            .expect("bridge id in scoped URL");

        let response = svc
            .handle_harness_mcp_request(
                bridge_id,
                &server.authorization_token,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/call",
                    "params": {
                        "name": "temps_native_permission",
                        "arguments": {
                            "tool_name": "Write",
                            "input": {"file_path": "/workspace/denied.txt"}
                        }
                    }
                }),
            )
            .await
            .expect("authorized capability")
            .expect("request response");

        let payload: serde_json::Value = serde_json::from_str(
            response["result"]["content"][0]["text"]
                .as_str()
                .expect("permission payload text"),
        )
        .expect("permission payload JSON");
        assert_eq!(payload["behavior"], "deny");
        assert_eq!(payload["message"], "Denied in Temps");
        assert!(payload.get("updatedInput").is_none());
    }
}
