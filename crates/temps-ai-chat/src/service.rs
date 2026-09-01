// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The conversation service: create/find/history + streaming `send_message`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, oneshot};

use chrono::Utc;
use futures::Stream;
use futures_util::StreamExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};

use temps_ai::{
    streaming::{PermissionDecision, PermissionKind},
    AiRequest, AiService, ChatMessage, ChatStreamDelta, ChatTool, ChatTurnRequest, ToolCall,
};

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
    pub sender: oneshot::Sender<PermissionDecision>,
    /// `public_id` of the conversation that registered this permission.
    pub conv_public_id: String,
    /// What kind of interaction the CLI subprocess is waiting for.
    pub kind: PermissionKind,
    /// The tool name from the original `control_request` (e.g. `"AskUserQuestion"`).
    /// Kept alongside `input` so a page reload can reconstruct the same
    /// interactive card instead of leaving the user with only the inert
    /// "asked" text message and no way to answer (ADR-038 Phase 2).
    pub tool_name: String,
    /// The original `control_request`'s `input` payload, verbatim.
    pub input: serde_json::Value,
    /// Unique registration generation. A cancelled older request must not
    /// remove a newer request that reused the same provider-supplied id.
    pub generation: uuid::Uuid,
}

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
use temps_auth::context::AuthContext;
use temps_entities::{ai_conversations, ai_messages};

use temps_ai_api_tools::{ApiCallScope, WriteApiToolsHandle, WritePrepareOutcome};

/// Owns a turn task for exactly as long as the returned SSE stream exists.
/// Browser Stop aborts the request, which drops the stream and therefore the
/// provider task instead of merely detaching it in the background.
struct AbortTurnOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortTurnOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

use crate::pending_actions::PendingActionService;
use crate::provider::ConversationContextProvider;
use crate::sensitive::redact_json_string;
use crate::ChatError;

/// Render a `control_request` (ADR-038 Phase 2) as human-readable text for the
/// synthetic `assistant` message persisted when a permission is asked, so a
/// page reload shows what was asked instead of just the eventual answer.
pub fn format_permission_asked(
    kind: &PermissionKind,
    tool_name: &str,
    input: &serde_json::Value,
) -> String {
    match kind {
        PermissionKind::Question => {
            let questions = input
                .get("questions")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if questions.is_empty() {
                return "Asked a question.".to_string();
            }
            questions
                .iter()
                .map(|q| {
                    let question = q
                        .get("question")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(question)");
                    let options: Vec<String> = q
                        .get("options")
                        .and_then(|v| v.as_array())
                        .map(|opts| {
                            opts.iter()
                                .filter_map(|o| {
                                    o.get("label").and_then(|l| l.as_str()).map(String::from)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if options.is_empty() {
                        format!("**{question}**")
                    } else {
                        format!("**{question}**\nOptions: {}", options.join(", "))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        }
        PermissionKind::PlanApproval => {
            let plan = input
                .get("plan")
                .and_then(|v| v.as_str())
                .unwrap_or("(no plan text provided)");
            format!("Proposed a plan:\n\n{plan}")
        }
        PermissionKind::ToolApproval => {
            format!("Requested to run tool **{tool_name}**.")
        }
    }
}

/// Render the user's [`PermissionDecision`] as human-readable text for the
/// synthetic `user` message persisted on resolve.
fn format_permission_answer(decision: &PermissionDecision) -> String {
    match decision {
        PermissionDecision::AllowTool => "Approved.".to_string(),
        PermissionDecision::DenyTool { reason } => match reason {
            Some(r) if !r.is_empty() => format!("Denied. {r}"),
            _ => "Denied.".to_string(),
        },
        PermissionDecision::AnswerQuestion { answers } => match answers.as_object() {
            Some(map) if !map.is_empty() => map
                .iter()
                .map(|(q, a)| {
                    let a = a
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| a.to_string());
                    format!("{q}: {a}")
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => "(no answer provided)".to_string(),
        },
        PermissionDecision::ApprovePlan => "Approved the plan.".to_string(),
        PermissionDecision::RejectPlan { feedback } => match feedback {
            Some(f) if !f.is_empty() => format!("Rejected the plan. {f}"),
            _ => "Rejected the plan.".to_string(),
        },
    }
}

/// Tool name for the write-proposal (confirm-gated) tool.
const TEMPS_WRITE_TOOL_NAME: &str = "temps_write";

/// Client-visible tool results must not contain raw data fetched through
/// server-side credentials. The model keeps the full result for reasoning;
/// the live stream and persisted transcript receive only this safe status.
fn public_tool_result(name: &str, result: &str) -> String {
    if name == TEMPS_WRITE_TOOL_NAME {
        return redact_json_string(result);
    }

    "Tool completed; detailed result is withheld from the chat transcript.".to_string()
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
    project_id: i32,
    provider: String,
    model: String,
    first_message: &str,
) {
    let req = AiRequest {
        purpose: "chat.title".to_string(),
        project_id: Some(project_id),
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
/// live-only.
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
    pub chat_enabled: bool,
    pub write_actions_enabled: bool,
}

/// Optional write-tool support wired into a `ConversationService` via
/// [`ConversationService::with_write_support`].
struct WriteSupport {
    write_handle: Arc<WriteApiToolsHandle>,
    pending: Arc<PendingActionService>,
}

/// Owns conversation persistence + AI turn streaming. Construct once with the
/// registered context providers; resolve via the plugin DI.
pub struct ConversationService {
    db: Arc<DatabaseConnection>,
    ai: Arc<dyn AiService>,
    providers: HashMap<&'static str, Arc<dyn ConversationContextProvider>>,
    /// Optional write-tool wiring. `None` until
    /// [`ConversationService::with_write_support`] is called, or when the
    /// project toggle is off — the `temps_write` tool is simply absent.
    write_support: Option<WriteSupport>,
    /// Reads operator-tunable chat limits. Consulted once per turn (the service
    /// caches, so this is not a per-turn database hit) rather than at startup,
    /// so changing the timeout in Settings takes effect on the next message
    /// instead of requiring a restart. `None` in tests and in any wiring that
    /// has not supplied it — the compiled default applies.
    config: Option<Arc<temps_config::ConfigService>>,
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
}

/// One event on a conversation's live wire, in the exact shape the SSE
/// handler already builds for `POST .../messages` — reusing this shape (not
/// the raw `ChatStreamEvent`) means the WS and SSE outputs can never drift:
/// both are built from the same `(event, data)` pair at the same call site.
#[derive(Debug, Clone)]
pub struct WireEvent {
    /// SSE/WS event name, e.g. `"token"` (implicit/unnamed for plain text in
    /// SSE), `"tool_call"`, `"permission_requested"`, `"user_message"`.
    pub event: String,
    /// The JSON (or plain text, for token deltas) payload.
    pub data: String,
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
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(Mutex::new(HashMap::new())),
        }
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
    /// is the common case (no other tab open) and not a failure — the SSE
    /// response to the sending tab is the primary delivery path regardless.
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

    /// Supply the settings service so operator-tuned chat limits apply.
    ///
    /// Optional: without it the compiled defaults are used, so a minimal wiring
    /// (and every test) still works without standing up config.
    pub fn with_config(mut self, config: Arc<temps_config::ConfigService>) -> Self {
        self.config = Some(config);
        self
    }

    /// Attach write-tool support (the `temps_write` tool + pending-action
    /// staging). This is called by the plugin after service construction once
    /// both the write handle and pending-action service are available.
    ///
    /// When not called (or when the project's `ai_write_actions_enabled` toggle
    /// is off), the service degrades gracefully: `temps_write` is not offered,
    /// no pending-action rows are created.
    pub fn with_write_support(
        mut self,
        write_handle: Arc<WriteApiToolsHandle>,
        pending: Arc<PendingActionService>,
    ) -> Self {
        self.write_support = Some(WriteSupport {
            write_handle,
            pending,
        });
        self
    }

    /// Is the selected AI provider configured? Feature opt-in is checked by
    /// the handler; transport-specific readiness stays behind `AiService`.
    pub async fn ai_available(&self) -> bool {
        self.ai.is_available().await
    }

    /// Load all independent gates for running an AI chat in a project.
    pub async fn chat_readiness(&self, project_id: i32) -> Result<ChatReadiness, ChatError> {
        let project = temps_entities::projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ChatError::ProjectLookup { project_id, source })?
            .ok_or(ChatError::ProjectNotFound(project_id))?;

        Ok(ChatReadiness {
            ai_configured: self.ai_available().await,
            chat_enabled: !matches!(project.ai_debug_chat_enabled, Some(false))
                || project.ai_write_actions_enabled,
            write_actions_enabled: project.ai_write_actions_enabled,
        })
    }

    /// The current creator's active conversation for a context, if one exists.
    pub async fn find_by_context(
        &self,
        project_id: i32,
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

    /// One creator's active conversations for a project, most-recently-active first.
    pub async fn list_conversations(
        &self,
        project_id: i32,
        user_id: i32,
    ) -> Result<Vec<ai_conversations::Model>, ChatError> {
        Ok(ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::Status.eq("active"))
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
    /// Legacy rows with `created_by = NULL` fail closed and are not returned.
    ///
    /// Conversations whose project has explicitly opted out of AI chat
    /// (`ai_debug_chat_enabled = false` without write actions) are EXCLUDED so
    /// a disabled project's chats never surface in the global switcher. This
    /// must mirror `ensure_chat_enabled` (the per-project gate): read-only
    /// chat is on by default, and a project with write actions on is always
    /// enabled, so those chats must appear here.
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
        let mut query = ai_conversations::Entity::find()
            .filter(ai_conversations::Column::CreatedBy.eq(user_id))
            .filter(ai_conversations::Column::Status.eq("active"))
            .order_by_desc(ai_conversations::Column::LastActivityAt)
            .limit(Self::LIST_ALL_LIMIT);
        if !hidden_project_ids.is_empty() {
            query = query.filter(
                ai_conversations::Column::ProjectId.is_not_in(hidden_project_ids.iter().copied()),
            );
        }
        let convs = query.all(self.db.as_ref()).await?;

        let mut ids: Vec<i32> = convs.iter().map(|c| c.project_id).collect();
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
        // Carry the toggle alongside name/slug so we can both annotate and filter.
        let by_id: HashMap<i32, (String, String, bool)> = projects
            .into_iter()
            .map(|p| {
                let enabled =
                    !matches!(p.ai_debug_chat_enabled, Some(false)) || p.ai_write_actions_enabled;
                (p.id, (p.name, p.slug, enabled))
            })
            .collect();

        Ok(convs
            .into_iter()
            .filter(|conversation| !hidden_project_ids.contains(&conversation.project_id))
            .filter_map(|c| {
                let info = by_id.get(&c.project_id).cloned();
                // Exclude any conversation whose project is missing or has the
                // toggle off — a disabled project's chats must not appear here.
                match info {
                    Some((name, slug, true)) => Some(ConversationWithProject {
                        project_name: Some(name),
                        project_slug: Some(slug),
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

    /// Find or create the current user's conversation for a context. Context
    /// identity is per creator, so project members never share stored results
    /// or resumable CLI sessions.
    #[allow(clippy::too_many_arguments)]
    pub async fn get_or_create(
        &self,
        project_id: i32,
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
            context_type: Set(context_type.to_string()),
            context_id: Set(context_id.to_string()),
            title: Set(seed.title.clone()),
            status: Set("active".to_string()),
            created_by: Set(Some(user_id)),
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
        project_id: i32,
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
        use temps_entities::{ai_gateway_config, ai_provider_keys};

        if let Some(preference) = ai_gateway_config::Entity::find()
            .filter(ai_gateway_config::Column::Scope.eq("instance"))
            .one(self.db.as_ref())
            .await?
        {
            if preference.provider_type == "agent_cli" {
                return preference.agent_cli_provider_id.ok_or_else(|| {
                    ChatError::Ai("active agent CLI preference has no provider id".to_string())
                });
            }
        }

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

    /// Persist the user's decision for a resolved interactive permission as a
    /// synthetic `user` message (ADR-038 Phase 2), so a page reload replays the
    /// question + answer instead of showing only the final assistant summary.
    /// Best-effort: a failure here must never fail `resolve_permission` — the
    /// subprocess has already been unblocked by the time this runs.
    pub async fn persist_permission_answered(
        &self,
        conversation_id: i64,
        decision: &PermissionDecision,
    ) {
        let content = format_permission_answer(decision);
        match self
            .insert_message(conversation_id, "user", &content, None)
            .await
        {
            Ok(m) => self.publish_wire_event(
                conversation_id,
                "user_message",
                serde_json::json!({
                    "content": m.content,
                    "created_at": m.created_at.to_rfc3339(),
                })
                .to_string(),
            ),
            Err(e) => {
                tracing::warn!(
                    conversation_id,
                    "failed to persist permission-answer message: {e}"
                );
            }
        }
    }

    /// Append a user message and stream the assistant reply. Persists the user
    /// message up front and the assistant message when the stream completes
    /// (the `system` seed is already the first stored turn, so history replay is
    /// the full context). Errors before streaming starts return `Err`; errors
    /// mid-stream arrive as a stream item.
    pub async fn send_message(
        &self,
        conv: &ai_conversations::Model,
        user_text: &str,
        // Optional client-supplied description of what the user is currently
        // viewing in the console (the page/entity). It is NOT persisted and NOT
        // shown in history — it's prepended to the user's message in-memory for
        // THIS turn only (see below), so the model can resolve "this trace" etc.
        page_context: Option<&str>,
        // The calling user's auth — forwarded to the tool loop so `call_api` can
        // replay GETs scoped to the user's own permissions.
        auth: &AuthContext,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>>, ChatError>
    {
        if !self.ai.is_available_for(Some(&conv.ai_provider)).await {
            return Err(ChatError::AiUnavailable);
        }
        let user_message = self
            .insert_message(conv.id, "user", user_text, None)
            .await?;
        self.touch(conv.id).await;

        // Cross-tab sync: a second tab watching this conversation never sees
        // the outgoing POST, so without this it has no way to learn a new
        // turn started or what the user typed. Best-effort (see
        // `publish_wire_event`) — never blocks or fails the turn.
        self.publish_wire_event(
            conv.id,
            "user_message",
            serde_json::json!({
                "content": user_message.content,
                "created_at": user_message.created_at.to_rfc3339(),
            })
            .to_string(),
        );

        let history = self.messages(conv.id).await?;

        // On the first user turn, generate an AI title from the message in the
        // background so the chat list shows a meaningful, content-derived label
        // instead of the generic seed title ("Project chat"). Fully decoupled
        // from the reply: a separate task that never blocks, holds open, or
        // fails the SSE stream, and runs at most once per conversation.
        if history.iter().filter(|m| m.role == "user").count() == 1 {
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

        // Gather the scoped tools available for this turn — the
        // context provider's own tools (e.g. a git-backed deployment can read
        // repo files) PLUS the shared, project-scoped trace tools (available in
        // every context when a trace store is configured) PLUS the ADR-024 generic
        // API meta-tools (search_api, describe_api, call_api) registered under the
        // sentinel context_type "__api_tools__". Gateway adapters receive
        // native function schemas; host adapters receive the same catalog via
        // their turn-scoped MCP bridge.
        let chat_capable = self.ai.chat_capable_for(Some(&conv.ai_provider)).await;
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
            if caller_may_use_repo_tools(auth) {
                if let Some(repo_tools_provider) = self.providers.get("__repo_tools__") {
                    tools.extend(
                        repo_tools_provider
                            .tools_with_auth(conv.project_id, &conv.context_id, auth)
                            .await,
                    );
                }
            }

            // Write tool: offered only when write support is wired AND the project
            // has opted in. Checking `ai_write_actions_enabled` here (once per turn,
            // from the already-loaded project row) ensures the model cannot stage
            // write proposals on a project that hasn't enabled the feature.
            let write_actions_enabled = self
                .load_write_actions_enabled(conv.project_id)
                .await
                .unwrap_or(false);
            let write_appendix = if write_actions_enabled {
                self.maybe_add_write_tool(&mut tools, &messages, auth)
            } else {
                None
            };
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

        // Every provider now enters the same turn runtime. The adapter decides
        // how to transport normalized text/tool/interaction events; chat owns
        // persistence, authorization, retries, and SSE for all providers.
        Ok(self
            .try_tool_loop(conv, messages, provider, tools, auth)
            .await)
    }

    async fn load_write_actions_enabled(&self, project_id: i32) -> Option<bool> {
        let project = temps_entities::projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .ok()??;
        Some(project.ai_write_actions_enabled)
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
            description: "Propose a mutation to the platform. \
                The change is NOT executed immediately — it creates a PROPOSAL that the user \
                must explicitly confirm in the UI before anything runs. \
                Use `--help` to discover write sections and operations exactly as with the \
                read-only `temps` tool, and ALWAYS read `<section> <operation> --help` to \
                confirm the operation does what the user actually asked BEFORE proposing it — \
                never pick an operation by its name alone (e.g. `promote_deployment` moves an \
                existing image to another environment; `rollback_to_deployment` reverts to an \
                older one; neither is a redeploy). If no available operation matches the \
                request, say so and ask — do NOT substitute a different operation. \
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
                Never claim the action has succeeded — tell the user to review and \
                confirm or reject the proposal."
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
                                        project_id is auto-filled. \
                                        This PROPOSES a change — it does NOT execute immediately."
                    },
                    "commands": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "An ORDERED list of write CLI command lines to propose as a \
                                        single multi-step plan (use instead of `command` when the \
                                        user asked for a sequence where order matters, e.g. \
                                        [\"update_environment_settings --env_id 8 --memory_limit 512\", \
                                        \"trigger_project_pipeline --environment_id 8\"]). Steps are \
                                        confirmed one at a time in this order; a step runs only after \
                                        the previous one succeeds. Provide exactly one of `command` or \
                                        `commands`."
                    }
                },
                "additionalProperties": false
            }),
        });
        if !help.trim().is_empty() {
            Some(format!(
                "## The `temps_write` confirm-gated mutation CLI\n\
                 You have a `temps_write` tool for proposing mutations. \
                 Every invocation ONLY stages a proposal — it does NOT execute. \
                 The user must confirm or reject each proposal in the UI. \
                 Never tell the user an action was taken; always direct them to confirm.\n\
                 Pick the operation that MATCHES the user's intent from the full list below \
                 (don't assume a verb lives in an obvious section — e.g. a redeploy/rebuild of \
                 a project is `trigger_project_pipeline`, not a `deployments` op). Read \
                 `<operation> --help` to verify flags, and never approximate with a \
                 similarly-named operation. Object/array flags must be strict JSON with \
                 double-quoted keys and string values, wrapped in single quotes. If nothing \
                 matches, say so and ask.\n\n\
                 Available write operations (permissions permitting):\n```\n{help}```"
            ))
        } else {
            Some("## The `temps_write` tool\nYou may propose confirm-gated mutations via `temps_write`. \
                 Each proposal must be confirmed in the UI before running."
                .to_string())
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
    /// The whole loop runs inside a task owned by the returned stream. Dropping
    /// the SSE stream aborts the task and upstream provider request. Completed
    /// turns persist `content` (all prose, for history replay) plus ordered
    /// `parts` (text/tool segments in occurrence order) and the executed `tools`,
    /// so a reload renders identically to the live stream.
    async fn try_tool_loop(
        &self,
        conv: &ai_conversations::Model,
        base_messages: Vec<ChatMessage>,
        provider: Option<Arc<dyn ConversationContextProvider>>,
        tools: Vec<ChatTool>,
        auth: &AuthContext,
    ) -> Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>> {
        // A turn is bounded by TIME, not by a number of steps.
        //
        // A step count is the wrong governor: it says nothing about cost or
        // about how long someone has been watching a spinner, and it cuts short
        // exactly the long, productive tasks people want the chat for. The user
        // can already see every tool call as it happens and press Stop, and
        // closing the panel drops the SSE stream, which cancels the upstream
        // request — so the interactive controls are the real ones. What a
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

        // Own everything the stream-owned task needs (the service borrows `&self`).
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
        let auth = auth.clone();
        // Write support clones (None when not wired or project toggle is off).
        let write_handle_opt = self
            .write_support
            .as_ref()
            .and_then(|ws| ws.write_handle.get());
        let pending_svc_opt = self.write_support.as_ref().map(|ws| ws.pending.clone());

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();

        // The loop, streaming, and persistence run in a stream-owned task. A
        // failed send lets the loop persist partial text; dropping the relay
        // stream aborts immediately and drops the upstream provider request.
        let turn_task = tokio::spawn(async move {
            let mut messages = base_messages;
            let execution_state = Arc::new(tokio::sync::Mutex::new(ToolExecutionState::default()));
            let interaction_db = db.clone();
            let interaction_conv_public_id = conv_public_id.clone();
            let interaction_registry = pending_permissions.clone();
            let interactions: temps_ai::InteractionExecutor = Arc::new(move |request| {
                let db = interaction_db.clone();
                let conv_public_id = interaction_conv_public_id.clone();
                let registry = interaction_registry.clone();
                let (sender, receiver) = tokio::sync::oneshot::channel();
                let generation = uuid::Uuid::new_v4();
                let entry = PendingPermissionEntry {
                    sender,
                    conv_public_id,
                    kind: request.kind.clone(),
                    tool_name: request.tool_name.clone(),
                    input: request.input.clone(),
                    generation,
                };
                match registry.lock() {
                    Ok(mut pending) => {
                        pending.insert(request.id.clone(), entry);
                    }
                    Err(poisoned) => {
                        poisoned.into_inner().insert(request.id.clone(), entry);
                    }
                }

                let content =
                    format_permission_asked(&request.kind, &request.tool_name, &request.input);
                let message = ai_messages::ActiveModel {
                    conversation_id: Set(conv_id),
                    role: Set("assistant".to_string()),
                    content: Set(content),
                    created_at: Set(Utc::now()),
                    ..Default::default()
                };
                let permission_id = request.id.clone();
                tokio::spawn(async move {
                    if let Err(error) = message.insert(db.as_ref()).await {
                        tracing::warn!(
                            conversation_id = conv_id,
                            permission_id = %permission_id,
                            "failed to persist provider interaction request: {error}"
                        );
                    }
                });

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
            // Did a round answer in prose (no tool calls)? Then we have the final
            // answer and stop; otherwise we may need a salvage call.
            let mut answered = false;
            // Set when a `tx.send` fails: the SSE receiver was dropped, i.e. the
            // client disconnected (navigated away, or pressed Stop). We stop
            // generating immediately — dropping the AI stream cancels the upstream
            // provider request, so a stopped turn doesn't keep costing tokens — and
            // still persist whatever streamed so far (the user turn isn't orphaned).
            let mut client_gone = false;
            // The last provider error seen while trying to produce this turn. Kept
            // so that a turn which ends up with nothing to show can explain WHY
            // instead of just stopping — see the empty-turn check after salvage.
            let mut last_provider_error: Option<String> = None;
            // Why generation stopped, when it was a bound rather than the model
            // finishing. Reported to the user — a turn that halts for a reason
            // nobody states looks identical to one that simply gave up.
            let mut stop_reason: Option<&'static str> = None;
            // Consecutive rounds in which every tool call was rejected.
            let mut unproductive_streak = 0usize;
            let turn_started = tokio::time::Instant::now();

            'rounds: for _ in 0..MAX_ROUNDS {
                if turn_started.elapsed() >= max_turn_duration {
                    stop_reason = Some(TURN_TIMEOUT_REASON);
                    break 'rounds;
                }
                let req = ChatTurnRequest {
                    purpose: format!("chat.{context_type}.tools"),
                    project_id: Some(project_id),
                    provider: Some(ai_provider.clone()),
                    model: Some(ai_model.clone()),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: Some(ai_permission_mode.clone()),
                    messages: messages.clone(),
                    tools: tools.clone(),
                    ..Default::default()
                };
                let executor_state = execution_state.clone();
                let executor_provider = provider.clone();
                let executor_api_tools = api_tools.clone();
                let executor_repo_tools = repo_tools.clone();
                let executor_write_handle = write_handle_opt.clone();
                let executor_pending = pending_svc_opt.clone();
                let executor_context_id = context_id.clone();
                let executor_auth = auth.clone();
                let executor: temps_ai::ToolExecutor = Arc::new(move |call: ToolCall| {
                    let state = executor_state.clone();
                    let provider = executor_provider.clone();
                    let api_tools = executor_api_tools.clone();
                    let repo_tools = executor_repo_tools.clone();
                    let write_handle = executor_write_handle.clone();
                    let pending = executor_pending.clone();
                    let context_id = executor_context_id.clone();
                    let auth = executor_auth.clone();
                    Box::pin(async move {
                        let mut state = state.lock().await;
                        Ok(dispatch_conversation_tool(
                            &call,
                            project_id,
                            conv_id,
                            &context_id,
                            &auth,
                            provider.as_ref(),
                            api_tools.as_ref(),
                            repo_tools.as_ref(),
                            write_handle.as_deref(),
                            pending.as_deref(),
                            &mut state,
                        )
                        .await)
                    })
                });
                // A single streaming pass: text deltas and tool calls arrive
                // inline. An error here (e.g. the model can't do tools) ends the
                // loop; the salvage below still tries a tool-free reply.
                let mut stream = match ai
                    .chat_stream_turn_with_services(
                        req,
                        temps_ai::TurnServices {
                            tools: Some(executor),
                            interactions: Some(interactions.clone()),
                        },
                    )
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("chat_stream_turn failed for conv {conv_id} (round): {e}");
                        last_provider_error = Some(e.to_string());
                        break 'rounds;
                    }
                };
                let mut round_text = String::new();
                let mut round_calls: Vec<ToolCall> = Vec::new();
                let mut native_tool_ids = std::collections::HashSet::new();
                // Did anything this round return usable data (vs. only rejections)?
                let mut round_produced_something = false;
                while let Some(item) = stream.next().await {
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
                                let _ = tx.send(Ok(ChatStreamEvent::Token(sep)));
                            }
                            round_text.push_str(&t);
                            content.push_str(&t);
                            cur_text.push_str(&t);
                            if tx.send(Ok(ChatStreamEvent::Token(t))).is_err() {
                                client_gone = true;
                                break;
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
                            if tx
                                .send(Ok(ChatStreamEvent::ToolCall {
                                    id: tc.id.clone(),
                                    name: tc.name.clone(),
                                    arguments: redact_json_string(&tc.arguments),
                                }))
                                .is_err()
                            {
                                client_gone = true;
                                break;
                            }
                            round_calls.push(tc);
                        }
                        Ok(ChatStreamDelta::ToolResult { call, result }) => {
                            native_tool_ids.insert(call.id.clone());
                            let display_arguments = redact_json_string(&call.arguments);
                            let display_result = redact_json_string(&result);
                            if tx
                                .send(Ok(ChatStreamEvent::ToolResult {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                    content: display_result.clone(),
                                }))
                                .is_err()
                            {
                                client_gone = true;
                                break;
                            }
                            let tool_part = serde_json::json!({
                                "id": call.id,
                                "name": call.name,
                                "arguments": display_arguments,
                                "result": display_result,
                            });
                            tools_meta.push(tool_part.clone());
                            parts.push(serde_json::json!({ "type": "tool", "tool": tool_part }));
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
                            if tx
                                .send(Ok(ChatStreamEvent::PermissionRequested {
                                    id: perm.id,
                                    kind: perm.kind,
                                    tool_name: perm.tool_name,
                                    input: perm.input,
                                }))
                                .is_err()
                            {
                                client_gone = true;
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("chat_stream_turn item error for conv {conv_id}: {e}");
                            // Provider subprocess failures arrive as stream items
                            // after `chat_stream_turn_with_executor` has returned.
                            // Preserve the concrete reason so an empty turn reports
                            // the authentication/model error instead of the generic
                            // "provider returned no response" fallback.
                            last_provider_error = Some(e.to_string());
                            break;
                        }
                    }
                }

                // Client disconnected mid-round — stop here. Dropping `stream` (the
                // AI token stream) at the end of this iteration cancels the upstream
                // provider request so generation actually stops.
                if client_gone {
                    break 'rounds;
                }

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
                            provider.as_ref(),
                            api_tools.as_ref(),
                            repo_tools.as_ref(),
                            write_handle_opt.as_deref(),
                            pending_svc_opt.as_deref(),
                            &mut state,
                        )
                        .await
                    };
                    let display_arguments = redact_json_string(&tc.arguments);
                    let display_result = public_tool_result(&tc.name, &result);
                    // Surface the result right after — live.
                    if tx
                        .send(Ok(ChatStreamEvent::ToolResult {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: display_result.clone(),
                        }))
                        .is_err()
                    {
                        client_gone = true;
                    }
                    let tool_part = serde_json::json!({
                        "id": tc.id.clone(),
                        "name": tc.name.clone(),
                        "arguments": display_arguments,
                        "result": display_result,
                    });
                    tools_meta.push(tool_part.clone());
                    parts.push(serde_json::json!({ "type": "tool", "tool": tool_part }));
                    if tool_result_is_productive(&result) {
                        round_produced_something = true;
                    }
                    messages.push(ChatMessage::tool(tc.id.clone(), result));
                }

                // The client went away while we were running tools — don't start
                // another (token-burning) round.
                if client_gone {
                    break 'rounds;
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
            if !answered && stop_reason.is_none() && !client_gone && !tools_meta.is_empty() {
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
                if tx.send(Ok(ChatStreamEvent::Token(note))).is_err() {
                    client_gone = true;
                }
            }

            // Salvage: the loop used tools but never settled on a prose answer (it
            // hit a bound while still calling tools). Make one tool-free streaming
            // call so the model answers from the evidence it gathered. Skip it if
            // the client is already gone — no one is listening.
            if !answered && !tools_meta.is_empty() && !client_gone {
                let mut final_messages = messages;
                final_messages.push(ChatMessage::user(FINAL_DIRECTIVE));
                let req = ChatTurnRequest {
                    purpose: format!("chat.{context_type}.tools.final"),
                    project_id: Some(project_id),
                    provider: Some(ai_provider.clone()),
                    model: Some(ai_model.clone()),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: Some(ai_permission_mode.clone()),
                    messages: final_messages,
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
                                let _ = tx.send(Ok(ChatStreamEvent::Token(sep)));
                            }
                            salvage_text.push_str(&t);
                            content.push_str(&t);
                            // Stop salvaging too if the client disconnects.
                            if tx.send(Ok(ChatStreamEvent::Token(t))).is_err() {
                                break;
                            }
                        }
                    }
                    if !salvage_text.is_empty() {
                        parts.push(serde_json::json!({ "type": "text", "text": salvage_text }));
                    }
                }
            }

            // A turn that produced nothing at all — no prose, no tool work — is
            // indistinguishable from a hung request in the UI: the user's own
            // message sits there and nothing ever arrives. That is the worst
            // possible outcome for a self-hosted operator with no support channel
            // to ask, so say what went wrong. The client already renders an
            // `error` SSE event; before this it simply never received one, because
            // a provider failure just ended the loop.
            //
            // Only when the client is still listening (`client_gone` means the
            // user navigated away or pressed Stop — an empty turn is expected and
            // explaining it to no one is pointless).
            if content.is_empty() && tools_meta.is_empty() && !client_gone {
                // `ChatError::Ai` already renders an "AI provider error: " prefix,
                // so the message continues that sentence rather than restating it.
                let detail = last_provider_error
                    .unwrap_or_else(|| "the provider returned no response".to_string());
                let _ = tx.send(Err(ChatError::Ai(format!(
                    "no reply was produced. {detail} \
                     Check the provider's key and model in Settings → AI Providers, \
                     then try again."
                ))));
            }

            // Persist the assistant turn once complete. `content` is the full prose
            // for history replay; `metadata.tools` + `metadata.parts` let the UI
            // replay the tool work and interleaving on reload. Skip an entirely
            // empty turn.
            if !content.is_empty() || !tools_meta.is_empty() {
                let mut meta = serde_json::Map::new();
                if !tools_meta.is_empty() {
                    meta.insert("tools".to_string(), serde_json::Value::Array(tools_meta));
                }
                if !parts.is_empty() {
                    meta.insert("parts".to_string(), serde_json::Value::Array(parts));
                }
                let metadata = if meta.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(meta))
                };
                let am = ai_messages::ActiveModel {
                    conversation_id: Set(conv_id),
                    role: Set("assistant".to_string()),
                    content: Set(content),
                    metadata: Set(metadata),
                    created_at: Set(Utc::now()),
                    ..Default::default()
                };
                if let Ok(msg) = am.insert(db.as_ref()).await {
                    // Best-effort: link any pending actions created during this turn
                    // to the persisted assistant message so the UI can correlate them.
                    let proposed_action_ids =
                        execution_state.lock().await.proposed_action_ids.clone();
                    if !proposed_action_ids.is_empty() {
                        if let Some(pending) = &pending_svc_opt {
                            if let Err(e) = pending.link_message(&proposed_action_ids, msg.id).await
                            {
                                tracing::warn!(
                                    conv_id,
                                    "Failed to link pending actions to message {}: {e}",
                                    msg.id
                                );
                            }
                        }
                    }
                }
            }
        });

        let out = async_stream::stream! {
            let _abort_on_drop = AbortTurnOnDrop(turn_task);
            while let Some(item) = rx.recv().await {
                yield item;
            }
        };
        Box::pin(out)
    }

    /// Archive a conversation (soft delete).
    pub async fn archive(&self, conv: &ai_conversations::Model) -> Result<(), ChatError> {
        let am = ai_conversations::ActiveModel {
            id: Set(conv.id),
            status: Set("archived".to_string()),
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

#[allow(clippy::too_many_arguments)]
async fn dispatch_conversation_tool(
    call: &ToolCall,
    project_id: i32,
    conversation_id: i64,
    context_id: &str,
    auth: &AuthContext,
    context_provider: Option<&Arc<dyn ConversationContextProvider>>,
    api_tools: Option<&Arc<dyn ConversationContextProvider>>,
    repo_tools: Option<&Arc<dyn ConversationContextProvider>>,
    write_handle: Option<&temps_ai_api_tools::InternalApiCaller>,
    pending: Option<&PendingActionService>,
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
            auth,
            write_handle,
            pending,
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
        if !caller_may_use_repo_tools(auth) {
            // Defence in depth: the tool was never offered to the model for
            // this caller, but a model can still emit the call name from
            // memory, and dispatch must not honour it.
            format!(
                "Tool '{}' is not available: it requires the {} permission.",
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

/// Dispatch a `temps_write` tool call: parse the command, validate (no
/// execution), create a pending-action row, return a JSON proposal receipt.
///
/// Returns a readable string result that goes back to the model as the tool
/// result — always, even on internal errors (never panics).
#[allow(clippy::too_many_arguments)]
async fn dispatch_write_tool(
    arguments: &str,
    project_id: i32,
    conversation_id: i64,
    auth: &AuthContext,
    write_handle: Option<&temps_ai_api_tools::InternalApiCaller>,
    pending_svc: Option<&PendingActionService>,
    proposed_action_ids: &mut Vec<i64>,
    seen_calls: &std::collections::HashMap<String, String>,
) -> String {
    let caller = match write_handle {
        Some(c) => c,
        None => {
            return "The `temps_write` tool is not available (write caller not yet wired or \
                    project toggle is off)."
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

    // Parse the JSON arguments.
    let args: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => return format!("Invalid `temps_write` arguments (not JSON): {e}"),
    };
    let scope = ApiCallScope {
        auth: auth.clone(),
        project_ids: vec![project_id],
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

    // Standalone single action (back-compat): one `create` row, no plan grouping.
    if !is_plan {
        let (prepared, perm) = &prepared_steps[0];
        return match pending
            .create(
                conversation_id,
                project_id,
                prepared,
                perm.clone(),
                Some(auth.user_id()),
            )
            .await
        {
            Ok(row) => {
                proposed_action_ids.push(row.id);
                serde_json::json!({
                    "status": "proposed",
                    "action_id": row.public_id,
                    "operation": row.operation_id,
                    "method": row.method,
                    "summary": row.summary,
                    "note": "PROPOSAL ONLY — awaiting explicit user confirmation in the UI. \
                             It has NOT run. Do not claim success; tell the user to review \
                             and confirm or reject it."
                })
                .to_string()
            }
            Err(e) => format!("Could not stage this change: {e}"),
        };
    }

    // Multi-step plan: one grouped set of rows, confirmed one step at a time.
    match pending
        .create_plan(
            conversation_id,
            project_id,
            &prepared_steps,
            Some(auth.user_id()),
        )
        .await
    {
        Ok(rows) => {
            for r in &rows {
                proposed_action_ids.push(r.id);
            }
            let steps: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "step": r.step_index + 1,
                        "action_id": r.public_id,
                        "operation": r.operation_id,
                        "method": r.method,
                        "summary": r.summary,
                    })
                })
                .collect();
            let plan_id = rows.first().and_then(|r| r.plan_public_id.clone());
            serde_json::json!({
                "status": "proposed_plan",
                "plan_id": plan_id,
                "step_count": rows.len(),
                "steps": steps,
                "note": "PROPOSAL ONLY — a multi-step plan awaiting the user's confirmation. \
                         NOTHING has run. The user confirms each step in order in the UI; a \
                         step runs only after the previous one succeeds, and a failed or \
                         rejected step halts the rest. Do not claim any step succeeded."
            })
            .to_string()
        }
        Err(e) => format!("Could not stage this plan: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    use sea_orm::{DatabaseBackend, MockDatabase};

    use temps_ai::{
        AiError, AiRequest, AiResponse, ChatStreamDelta, ChatTurnStream, TokenStream, ToolCall,
    };

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

    /// A stub provider exposing a single `echo` tool, counting executions.
    struct StubProvider {
        tool_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    struct AuthRecordingProvider {
        seen_user_id: Arc<std::sync::atomic::AtomicI32>,
    }

    struct SeedOnlyProvider;

    #[async_trait]
    impl ConversationContextProvider for StubProvider {
        fn context_type(&self) -> &'static str {
            "test"
        }
        async fn seed(
            &self,
            _project_id: i32,
            _context_id: &str,
        ) -> Option<crate::provider::ConversationSeed> {
            None
        }
        async fn tools(&self, _project_id: i32, _context_id: &str) -> Vec<ChatTool> {
            vec![ChatTool {
                name: "echo".to_string(),
                description: "Echoes its input.".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }]
        }
        async fn execute_tool(
            &self,
            _project_id: i32,
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
            _project_id: i32,
            _context_id: &str,
        ) -> Option<crate::provider::ConversationSeed> {
            None
        }

        async fn execute_tool_with_auth(
            &self,
            _project_id: i32,
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
            _project_id: i32,
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
            project_id: 7,
            context_type: "test".to_string(),
            context_id: "42".to_string(),
            title: None,
            status: "active".to_string(),
            created_by: None,
            metadata: None,
            cli_session_id: None,
            cli_session_fingerprint: None,
            ai_provider: "gateway".to_string(),
            ai_model: "gpt-4o-mini".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "confirm-actions".to_string(),
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
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
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
            7,
            1,
            "42",
            &auth,
            None,
            Some(&provider),
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
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    fn db_service_with_ai(db: DatabaseConnection, ai: Arc<dyn AiService>) -> ConversationService {
        ConversationService {
            db: Arc::new(db),
            ai,
            providers: HashMap::new(),
            write_support: None,
            config: None,
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
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
            project_id,
            context_type: "deployment".to_string(),
            context_id: "1".to_string(),
            title: Some("t".to_string()),
            status: "active".to_string(),
            created_by: Some(5),
            metadata: None,
            cli_session_id: None,
            cli_session_fingerprint: None,
            ai_provider: "gateway".to_string(),
            ai_model: "gpt-4o-mini".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "confirm-actions".to_string(),
            created_at: now,
            last_activity_at: now,
        }
    }

    fn agent_cli_preference(provider_id: Option<&str>) -> temps_entities::ai_gateway_config::Model {
        let now = Utc::now();
        temps_entities::ai_gateway_config::Model {
            id: 1,
            scope: "instance".to_string(),
            allowed_models: None,
            max_requests_per_minute: None,
            max_cost_per_month_microcents: None,
            created_at: now,
            updated_at: now,
            provider_type: "agent_cli".to_string(),
            agent_cli_provider_id: provider_id.map(str::to_string),
            interactive_bridge_enabled: false,
            summary_provider_id: None,
            summary_model: None,
            summary_thinking_level: None,
        }
    }

    #[tokio::test]
    async fn omitted_provider_uses_active_agent_cli_preference() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[agent_cli_preference(Some("opencode"))]])
            .into_connection();
        let service = db_service(db);

        assert_eq!(
            service
                .resolve_default_provider()
                .await
                .expect("active CLI preference"),
            "opencode"
        );
    }

    #[tokio::test]
    async fn active_agent_cli_preference_requires_a_provider_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[agent_cli_preference(None)]])
            .into_connection();
        let service = db_service(db);

        let error = service
            .resolve_default_provider()
            .await
            .expect_err("invalid preference must fail closed");
        assert!(matches!(error, ChatError::Ai(message) if message.contains("no provider id")));
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

    /// Build a minimal valid `projects::Model` carrying a chosen
    /// `ai_debug_chat_enabled` toggle.
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
            error_source_root: None,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
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
        user_11.created_by = Some(11);
        user_11.ai_provider = "gateway_key:1".to_string();
        let mut user_22 = conv_for(2, 7, "user22");
        user_22.created_by = Some(22);
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
            pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        };

        let first = svc
            .get_or_create(
                7,
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
                7,
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

        assert_eq!(first.created_by, Some(11));
        assert_eq!(second.created_by, Some(22));
        assert_ne!(first.public_id, second.public_id);
    }

    #[tokio::test]
    async fn chat_readiness_reports_independent_gates() {
        let mut project = project_with_toggle(7, "Alpha", "alpha", Some(false));
        project.ai_write_actions_enabled = true;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![project]])
            .into_connection();

        let readiness = db_service(db).chat_readiness(7).await.expect("readiness");
        assert_eq!(
            readiness,
            ChatReadiness {
                ai_configured: true,
                chat_enabled: true,
                write_actions_enabled: true,
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
        assert!(readiness.chat_enabled);
        assert!(!readiness.write_actions_enabled);
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
            .find_by_context(7, 5, "deployment", "1")
            .await
            .expect("query ok");
        let conv = found.expect("a conversation should be found");
        assert_eq!(conv.project_id, 7);
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
            .find_by_context(7, 5, "deployment", "1")
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
            .find_by_context(7, 11, "deployment", "1")
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
        assert!(convs.iter().all(|c| c.project_id == 7));
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
            .find(|i| i.conversation.project_id == 7)
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

    // list_all_conversations: a conversation whose project has explicitly opted
    // out (toggle = false) is EXCLUDED from the global switcher, even though its
    // row is active. NULL means default-on, so those chats stay visible.
    #[tokio::test]
    async fn test_list_all_conversations_excludes_disabled_projects() {
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
            2,
            "explicit opt-out is excluded; default (NULL) stays visible"
        );
        assert!(items
            .iter()
            .all(|i| i.conversation.public_id != "pubDisabled"));
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
        assert_eq!(conv.project_id, 7);
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

        let (tx, rx) = oneshot::channel::<PermissionDecision>();
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
        entry.sender.send(PermissionDecision::AllowTool).unwrap();
        let decision = rx.await.unwrap();
        assert!(matches!(decision, PermissionDecision::AllowTool));
    }

    /// Double-resolve: the second remove returns None (409 semantic).
    #[tokio::test]
    async fn test_registry_double_resolve_returns_none() {
        use temps_ai::streaming::{PermissionDecision, PermissionKind};
        use tokio::sync::oneshot;

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = db_service(db);

        let (tx, _rx) = oneshot::channel::<PermissionDecision>();
        svc.pending_permissions.lock().unwrap().insert(
            "req-dup".to_string(),
            PendingPermissionEntry {
                sender: tx,
                conv_public_id: "pub-dup".to_string(),
                kind: PermissionKind::ToolApproval,
                tool_name: "Bash".to_string(),
                input: serde_json::Value::Null,
                generation: uuid::Uuid::new_v4(),
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

        let (tx, rx) = oneshot::channel::<PermissionDecision>();
        svc.pending_permissions.lock().unwrap().insert(
            "req-drain".to_string(),
            PendingPermissionEntry {
                sender: tx,
                conv_public_id: "pub-drain".to_string(),
                kind: PermissionKind::ToolApproval,
                tool_name: "Bash".to_string(),
                input: serde_json::Value::Null,
                generation: uuid::Uuid::new_v4(),
            },
        );

        // Simulate subprocess exit: drain the registry with a synthetic deny.
        let drained: Vec<_> = {
            let mut registry = svc.pending_permissions.lock().unwrap();
            registry.drain().collect()
        };
        assert_eq!(drained.len(), 1, "one entry must have been drained");
        let (_id, entry) = drained.into_iter().next().unwrap();
        let deny_result = entry.sender.send(PermissionDecision::DenyTool {
            reason: Some("subprocess exited".to_string()),
        });
        assert!(deny_result.is_ok(), "send on drained entry must succeed");

        let decision = rx.await.unwrap();
        assert!(
            matches!(decision, PermissionDecision::DenyTool { reason: Some(ref r) } if r.contains("exited")),
            "drained entry must deliver deny: {decision:?}"
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
            msg.contains("AI Providers"),
            "the error must point at the place that fixes it: {msg}"
        );
    }
}
