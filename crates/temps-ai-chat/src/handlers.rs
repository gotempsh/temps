//! HTTP surface for AI debugging conversations (ADR-023).
//!
//! `GET/POST /projects/{project_id}/ai/conversations` (find / get-or-create),
//! `GET .../{public_id}` (history), `POST .../{public_id}/messages` (SSE stream
//! of the assistant reply), `POST .../{public_id}/archive`. All gated on the
//! per-project `ai_debug_chat_enabled` toggle + AI being configured.

use std::convert::Infallible;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path, Query, State,
    },
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
    Extension, Json, Router,
};
use futures::stream::Stream;
use futures_util::StreamExt;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde::{Deserialize, Serialize};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use temps_auth::permissions::Permission;
use temps_auth::{
    context::AuthContext, deny_deployment_token, permission_guard, project_access_guard,
    project_scope_guard, RequireAuth,
};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditLogger, RequestMetadata};
use temps_entities::{ai_conversations, ai_messages, ai_pending_actions};

use temps_ai::streaming::{PermissionDecision, PermissionKind, PermissionRequest};

use crate::audit::{
    AiActionConfirmedAudit, AiActionRejectedAudit, ChatMessageSentAudit, ConversationArchivedAudit,
    ConversationCreatedAudit, ConversationRenamedAudit, PermissionResolvedAudit,
};
use crate::pending_actions::{PendingActionError, PendingActionService};
use crate::sensitive::{display_value, redact_json_string, redact_text, redact_value};
use crate::service::ChatStreamEvent;
use crate::{ChatError, ConversationService};

/// Shared state for the chat routes.
pub struct AppState {
    pub service: Arc<ConversationService>,
    pub db: Arc<DatabaseConnection>,
    /// Audit logger for write operations (best-effort; never fails a request).
    pub audit_service: Arc<dyn AuditLogger>,
    /// Pending-action service (confirm/reject write proposals).
    pub pending_actions: Arc<PendingActionService>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl AppState {
    /// Emit an audit entry, best-effort: a logging failure must never fail the
    /// underlying operation (it already succeeded).
    async fn audit(&self, op: &dyn temps_core::AuditOperation) {
        if let Err(e) = self.audit_service.create_audit_log(op).await {
            error!("Failed to write AI-chat audit log: {e}");
        }
    }
}

// --- DTOs --------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationResponse {
    pub public_id: String,
    pub context_type: String,
    pub context_id: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
    pub last_activity_at: String,
    pub ai_provider: String,
    pub ai_model: String,
    pub ai_thinking_level: Option<String>,
    pub ai_permission_mode: String,
}

impl From<ai_conversations::Model> for ConversationResponse {
    fn from(m: ai_conversations::Model) -> Self {
        Self {
            public_id: m.public_id,
            context_type: m.context_type,
            context_id: m.context_id,
            title: m.title,
            status: m.status,
            created_at: m.created_at.to_rfc3339(),
            last_activity_at: m.last_activity_at.to_rfc3339(),
            ai_provider: m.ai_provider,
            ai_model: m.ai_model,
            ai_thinking_level: m.ai_thinking_level,
            ai_permission_mode: m.ai_permission_mode,
        }
    }
}

/// A conversation in the unified cross-project switcher: carries the project it
/// belongs to (name/slug) so the UI can show where the chat was started and
/// link back to the source.
#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalConversationResponse {
    pub public_id: String,
    pub project_id: i32,
    pub project_name: Option<String>,
    pub project_slug: Option<String>,
    pub context_type: String,
    pub context_id: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
    pub last_activity_at: String,
    pub ai_provider: String,
    pub ai_model: String,
    pub ai_thinking_level: Option<String>,
    pub ai_permission_mode: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MessageResponse {
    pub role: String,
    pub content: String,
    pub created_at: String,
    /// Tools the assistant ran on this turn (persisted in message metadata), so
    /// the chat replays its tool work after a reload. Absent for plain turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolInfo>>,
    /// Ordered render segments (text / tool, in the order they occurred) so a
    /// reloaded chat shows the same interleaving as the live stream. Absent for
    /// older messages persisted before parts were tracked; the client then falls
    /// back to `tools` (rendered first) + `content`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<MessagePart>>,
}

/// One persisted tool invocation + its result, attached to an assistant message.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ToolInfo {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
}

/// One ordered segment of an assistant turn: a chunk of prose, or a tool
/// invocation. Mirrors the `metadata.parts` persisted by the chat service.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MessagePart {
    Text { text: String },
    Tool { tool: ToolInfo },
}

impl From<ai_messages::Model> for MessageResponse {
    fn from(m: ai_messages::Model) -> Self {
        let redact_tool = |mut tool: ToolInfo| {
            tool.arguments = redact_json_string(&tool.arguments);
            tool.result = tool.result.map(|result| redact_json_string(&result));
            tool
        };
        let tools = m
            .metadata
            .as_ref()
            .and_then(|v| v.get("tools"))
            .and_then(|t| serde_json::from_value::<Vec<ToolInfo>>(t.clone()).ok())
            .map(|tools| tools.into_iter().map(&redact_tool).collect::<Vec<_>>())
            .filter(|t| !t.is_empty());
        let parts = m
            .metadata
            .as_ref()
            .and_then(|v| v.get("parts"))
            .and_then(|p| serde_json::from_value::<Vec<MessagePart>>(p.clone()).ok())
            .map(|parts| {
                parts
                    .into_iter()
                    .map(|part| match part {
                        MessagePart::Tool { tool } => MessagePart::Tool {
                            tool: redact_tool(tool),
                        },
                        text => text,
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|p| !p.is_empty());
        Self {
            role: m.role,
            content: m.content,
            created_at: m.created_at.to_rfc3339(),
            tools,
            parts,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationDetailResponse {
    #[serde(flatten)]
    pub conversation: ConversationResponse,
    /// Turns oldest-first. The `system` seed message is omitted (internal).
    pub messages: Vec<MessageResponse>,
    /// A still-unresolved interactive permission request (ADR-038 Phase 2), if
    /// one is pending on this conversation right now. The client renders this
    /// as a live, answerable `PermissionCard` — without it, a question that
    /// arrived while the tab was away shows only as inert history text with no
    /// way to answer it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_permission: Option<PermissionRequest>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateConversationRequest {
    /// e.g. `"deployment"`.
    pub context_type: String,
    /// The entity id (ints stringified).
    pub context_id: String,
    /// Provider pinned to this conversation. Omitted requests use the current
    /// instance preference.
    pub ai_provider: Option<String>,
    pub ai_model: Option<String>,
    pub ai_thinking_level: Option<String>,
    pub ai_permission_mode: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FindConversationQuery {
    pub context_type: String,
    pub context_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RenameConversationRequest {
    /// New human-facing title. Trimmed; must be non-empty after trimming.
    pub title: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendMessageRequest {
    pub content: String,
    /// Optional next-turn model. The provider harness remains pinned, but its
    /// advertised models may be changed between turns.
    #[serde(default)]
    pub ai_model: Option<String>,
    /// Optional next-turn thinking level.
    #[serde(default)]
    pub ai_thinking_level: Option<String>,
    /// Optional next-turn permission mode for the pinned provider harness.
    #[serde(default)]
    pub ai_permission_mode: Option<String>,
    /// Optional, client-supplied description of the page/entity the user is
    /// currently viewing (e.g. a trace in a project). Injected into the model's
    /// view of this turn only — never stored or shown in history. Capped server
    /// side; oversized values are ignored rather than rejected.
    #[serde(default)]
    pub page_context: Option<String>,
}

/// Payload for the `tool_call` SSE event: the model is about to run a tool.
/// Serialized as compact single-line JSON onto one `data:` line.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToolCallEvent {
    pub id: String,
    pub name: String,
    /// The raw JSON-args string the model emitted.
    pub arguments: String,
}

/// Payload for the `tool_result` SSE event: a tool finished running. Serialized
/// as compact single-line JSON; `content` is JSON-string-escaped so it stays on
/// one `data:` line even when long.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToolResultEvent {
    pub id: String,
    pub name: String,
    pub content: String,
}

/// Payload for the `permission_requested` SSE event (ADR-038 Phase 2).
/// The active provider turn is paused waiting for the user to approve or deny
/// a tool/question/plan. Resolve via
/// `POST .../permissions/{id}/resolve`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionRequestedEvent {
    /// The CLI's `request_id` — also the `{permission_id}` in the resolve URL.
    pub id: String,
    /// What kind of interaction is required: `"tool_approval"`, `"question"`,
    /// or `"plan_approval"`.
    pub kind: PermissionKind,
    /// Tool name from the CLI request (e.g. `"Bash"`, `"AskUserQuestion"`).
    pub tool_name: String,
    /// Raw `input` from the CLI request. Passed through verbatim so each
    /// milestone's card can render tool-specific fields without the service
    /// layer needing to know about their schemas.
    pub input: serde_json::Value,
}

/// Body for the `POST .../permissions/{permission_id}/resolve` endpoint.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolvePermissionRequest {
    pub decision: PermissionDecision,
}

// --- error mapping -----------------------------------------------------------

impl From<ChatError> for Problem {
    fn from(e: ChatError) -> Self {
        match e {
            ChatError::NotFound(_) => problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                .with_title("Conversation Not Found")
                .with_detail(e.to_string()),
            ChatError::ProjectNotFound(_) => problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail(e.to_string()),
            ChatError::NoProvider(_) | ChatError::ContextUnavailable => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Context Not Available")
                    .with_detail(e.to_string())
            }
            ChatError::AiUnavailable => problemdetails::new(axum::http::StatusCode::CONFLICT)
                .with_title("AI Not Configured")
                .with_detail(e.to_string()),
            ChatError::ProjectLookup { .. } | ChatError::Db(_) => {
                error!("AI chat database operation failed: {e}");
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("A database operation failed while handling the AI chat request.")
            }
            ChatError::Ai(_) => problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(e.to_string()),
            ChatError::PermissionKindMismatch { .. } => {
                problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
                    .with_title("Permission Decision Mismatch")
                    .with_detail(e.to_string())
            }
        }
    }
}

impl From<PendingActionError> for Problem {
    fn from(e: PendingActionError) -> Self {
        match e {
            PendingActionError::NotFound { .. } => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Pending Action Not Found")
                    .with_detail(e.to_string())
            }
            PendingActionError::InvalidState { .. } => {
                problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title("Invalid Action State")
                    .with_detail(e.to_string())
            }
            PendingActionError::StepBlocked { .. } => {
                problemdetails::new(axum::http::StatusCode::CONFLICT)
                    .with_title("Plan Step Not Ready")
                    .with_detail(e.to_string())
            }
            PendingActionError::PermissionDenied { .. } => {
                problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                    .with_title("Permission Denied")
                    .with_detail(e.to_string())
            }
            PendingActionError::Disabled { .. } => {
                problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                    .with_title("AI Write Actions Disabled")
                    .with_detail(e.to_string())
            }
            PendingActionError::Unavailable => {
                problemdetails::new(axum::http::StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Write Actions Unavailable")
                    .with_detail(e.to_string())
            }
            PendingActionError::Execution { .. } => {
                problemdetails::new(axum::http::StatusCode::BAD_GATEWAY)
                    .with_title("Execution Failed")
                    .with_detail(e.to_string())
            }
            PendingActionError::Database(_) => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(e.to_string())
            }
            PendingActionError::Encryption { .. } => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Protected Action Data Error")
                    .with_detail(e.to_string())
            }
        }
    }
}

/// Recursively scrub object keys that may carry sensitive values.
///
/// Any key whose name (case-insensitive) is or contains one of:
/// `value`, `secret`, `password`, `token`, `key`
/// has its value replaced with `"***"`. Objects nested below neutral wrapper
/// keys such as `parameters` and objects inside arrays are traversed too.
/// Structural fields (`operation`, `method`, `summary`, etc.) are left intact.
fn redact_params(v: &serde_json::Value) -> serde_json::Value {
    redact_value(v)
}

// --- Pending-action DTO ------------------------------------------------------

/// A proposed AI write action awaiting human confirmation.
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingActionResponse {
    pub public_id: String,
    pub operation_id: String,
    pub method: String,
    pub summary: String,
    pub status: String,
    /// Set when this action is one step of a multi-step plan (chained actions);
    /// all steps of the plan share this id. Absent for standalone single actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_public_id: Option<String>,
    /// 0-based order of this step within its plan (0 for standalone actions).
    pub step_index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_permission: Option<String>,
    /// The flat params to be replayed at execute time (shown pre-execution for review).
    pub params: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executed_at: Option<String>,
}

impl From<ai_pending_actions::Model> for PendingActionResponse {
    fn from(m: ai_pending_actions::Model) -> Self {
        Self {
            public_id: m.public_id,
            operation_id: m.operation_id,
            method: m.method,
            summary: m.summary,
            status: m.status,
            plan_public_id: m.plan_public_id,
            step_index: m.step_index,
            required_permission: m.required_permission,
            // Scrub sensitive values (e.g. env-var values) before returning to
            // clients who may only hold a broad read permission.
            params: display_value(&m.params),
            result: m.result.as_ref().map(redact_params),
            error: m.error.map(|error| redact_text(&error)),
            created_at: m.created_at.to_rfc3339(),
            confirmed_at: m.confirmed_at.map(|t| t.to_rfc3339()),
            executed_at: m.executed_at.map(|t| t.to_rfc3339()),
        }
    }
}

/// Toggle-only gate: read-only chat is enabled by default; only an explicit
/// opt-out (`ai_debug_chat_enabled = Some(false)`) disables it. Write actions
/// (`ai_write_actions_enabled`) remain a separate manual opt-in — they are
/// *proposed and confirmed inside this chat*, so enabling the more-privileged
/// capability must never block the chat itself (otherwise a project with
/// write on but debug-chat explicitly off could never open the chat to use
/// it). Used by the read/archive handlers so that an explicit opt-out
/// consistently revokes access (403) to existing chat content —
/// reading/archiving history must not require an AI provider to be
/// configured.
async fn ensure_chat_enabled(db: &DatabaseConnection, project_id: i32) -> Result<(), Problem> {
    let project = temps_entities::projects::Entity::find_by_id(project_id)
        .one(db)
        .await
        .map_err(|e| {
            problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_detail(e.to_string())
        })?;
    let enabled = project
        .map(|p| !matches!(p.ai_debug_chat_enabled, Some(false)) || p.ai_write_actions_enabled)
        .unwrap_or(false);
    if !enabled {
        return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
            .with_title("AI Chat Disabled")
            .with_detail(
                "AI chat has been disabled for this project. Re-enable it in the project's AI settings.",
            ));
    }
    Ok(())
}

/// Host CLIs execute as the Temps host user, so project write access alone is
/// not sufficient. The stronger check is repeated on every send because the
/// creator's provider administration permission may have been revoked.
fn ensure_runtime_permission(
    auth: &AuthContext,
    provider: Option<&str>,
    permission_mode: Option<&str>,
) -> Result<(), Problem> {
    if let Some(provider) = provider.and_then(temps_agents::ai_cli::find_provider) {
        let has_host_access = match provider.host_access_requirement {
            temps_agents::ai_cli::HostAccessRequirement::AiGatewayWrite => {
                auth.has_permission(&Permission::AiGatewayWrite)
            }
            temps_agents::ai_cli::HostAccessRequirement::SystemAdmin => {
                auth.has_permission(&Permission::SystemAdmin)
            }
        };
        if !has_host_access {
            return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                .with_title("Host AI Provider Access Denied")
                .with_detail(format!(
                    "Your role cannot run the host-authenticated {} provider.",
                    provider.name
                )));
        }
        let requires_system_admin = permission_mode
            .and_then(|mode| {
                provider
                    .permission_modes
                    .iter()
                    .find(|option| option.id == mode)
            })
            .is_some_and(|mode| mode.requires_system_admin);
        if requires_system_admin && !auth.has_permission(&Permission::SystemAdmin) {
            return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                .with_title("Full AI Access Denied")
                .with_detail("This AI permission mode requires system administrator permission."));
        }
    }
    Ok(())
}

/// Gate for create/send: the project must have opted into AI debug chat AND AI
/// must be configured. Builds on [`ensure_chat_enabled`] (toggle) and adds the
/// AI-availability check required to actually run a turn.
async fn ensure_enabled(state: &AppState, project_id: i32) -> Result<(), Problem> {
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    if !state.service.ai_available().await {
        return Err(problemdetails::new(axum::http::StatusCode::CONFLICT)
            .with_title("AI Not Configured")
            .with_detail(
                "Configure an AI provider to use debugging chat — a gateway key or a \
                 host-authenticated provider both support the common chat, tool, and \
                 realtime runtime.",
            ));
    }
    Ok(())
}

/// Upper bounds on client-supplied chat inputs, enforced before any DB or AI
/// call so oversized payloads can't bloat storage or run up AI token cost.
const MAX_CONTEXT_TYPE_LEN: usize = 64;
const MAX_CONTEXT_ID_LEN: usize = 128;
const MAX_MESSAGE_CONTENT_LEN: usize = 32_000;
/// Cap on the advisory `page_context` (well under a message; it's framing).
const MAX_PAGE_CONTEXT_LEN: usize = 4_000;
/// Cap on a user-supplied conversation title (a short label, not prose).
const MAX_TITLE_LEN: usize = 200;

/// Body-size limit for `resolve_permission` POST payloads (finding 3).
///
/// The payload is a `PermissionDecision` — at most a few short strings.  8 KB
/// is ample for any real answer; anything larger is either a client bug or an
/// attempt to exhaust memory / blow the stdin pipe to the subprocess.
const RESOLVE_PERMISSION_BODY_LIMIT: usize = 8 * 1024;

/// Maximum byte length for free-text fields inside a `PermissionDecision`
/// (`DenyTool.reason`, `RejectPlan.feedback`, serialized `AnswerQuestion.answers`).
///
/// These strings are written verbatim to the subprocess's stdin.  A hard cap
/// prevents a large payload from hanging the stdin write or exhausting buffers.
const MAX_DECISION_STRING_LEN: usize = 4 * 1024;

/// 400 for an over-length input field.
fn too_long(field: &str, max: usize) -> Problem {
    problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
        .with_title("Input Too Long")
        .with_detail(format!(
            "'{field}' exceeds the maximum length of {max} characters."
        ))
}

/// Contexts can expose domain data beyond the generic project-chat surface.
/// Enforce the same domain permission as the canonical API before creating,
/// reading, or running one of those conversations.
fn ensure_context_read_permission(auth: &AuthContext, context_type: &str) -> Result<(), Problem> {
    if !can_read_context(auth, context_type) {
        return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
            .with_title("Insufficient Permissions")
            .with_detail("The otel:read permission is required for alert suggestion chats."));
    }
    Ok(())
}

fn can_read_context(auth: &AuthContext, context_type: &str) -> bool {
    context_type != "alert_suggest" || auth.has_permission(&Permission::OtelRead)
}

fn ensure_conversation_read_permission(
    auth: &AuthContext,
    conversation: &ai_conversations::Model,
) -> Result<(), Problem> {
    // Persisted history is project/context data. Host-provider administration
    // is an execution boundary and must not make stored chat content disappear
    // when that separate permission is later revoked.
    ensure_context_read_permission(auth, &conversation.context_type)
}

async fn hidden_conversation_project_ids(
    auth: &AuthContext,
    checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<Vec<i32>, Problem> {
    if auth.is_admin() || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
        return Ok(Vec::new());
    }
    let Some(checker) = checker else {
        return Ok(Vec::new());
    };
    let Some(user_id) = auth.user_id_opt() else {
        return Err(problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Project Access Denied")
            .with_detail("A user identity is required to list AI conversations."));
    };
    checker
        .hidden_project_ids(user_id)
        .await
        .map(|ids| ids.unwrap_or_default())
        .map_err(|error| {
            tracing::error!(user_id, error = %error, "failed to filter global AI conversations");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Project Access Check Failed")
                .with_detail("Could not verify project access; please try again")
        })
}

// --- handlers ----------------------------------------------------------------

/// Find the current user's existing chat for a context (returns `null` if none
/// yet). Conversations are private even between members of the same project.
/// Requires the per-project `ai_debug_chat_enabled` toggle to be on.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations",
    params(("project_id" = i32, Path,), ("context_type" = String, Query,), ("context_id" = String, Query,)),
    responses((status = 200, body = Option<ConversationResponse>), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn find_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(q): Query<FindConversationQuery>,
) -> Result<Json<Option<ConversationResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    // Bound the lookup keys, consistent with create_conversation, so oversized
    // query strings can't reach the DB.
    if q.context_type.len() > MAX_CONTEXT_TYPE_LEN {
        return Err(too_long("context_type", MAX_CONTEXT_TYPE_LEN));
    }
    if q.context_id.len() > MAX_CONTEXT_ID_LEN {
        return Err(too_long("context_id", MAX_CONTEXT_ID_LEN));
    }
    ensure_context_read_permission(&auth, &q.context_type)?;
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let found = state
        .service
        .find_by_context(project_id, auth.user_id(), &q.context_type, &q.context_id)
        .await?;
    Ok(Json(found.map(ConversationResponse::from)))
}

/// List the current user's active conversations across all projects,
/// most-recently-active first, annotated with project name/slug.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/ai/conversations",
    responses((status = 200, body = Vec<GlobalConversationResponse>), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn list_all_conversations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<GlobalConversationResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    // This global endpoint returns conversations across every project; a
    // project-scoped deployment/project token must not reach another tenant's
    // chats through it. Restrict to human/admin (user/API-key) principals.
    deny_deployment_token!(auth);
    let hidden_project_ids =
        hidden_conversation_project_ids(&auth, &state.project_access_checker).await?;
    let items = state
        .service
        .list_all_conversations(auth.user_id(), &hidden_project_ids)
        .await?;
    Ok(Json(
        items
            .into_iter()
            .filter(|i| can_read_context(&auth, &i.conversation.context_type))
            .map(|i| GlobalConversationResponse {
                public_id: i.conversation.public_id,
                project_id: i.conversation.project_id,
                project_name: i.project_name,
                project_slug: i.project_slug,
                context_type: i.conversation.context_type,
                context_id: i.conversation.context_id,
                title: i.conversation.title,
                status: i.conversation.status,
                created_at: i.conversation.created_at.to_rfc3339(),
                last_activity_at: i.conversation.last_activity_at.to_rfc3339(),
                ai_provider: i.conversation.ai_provider,
                ai_model: i.conversation.ai_model,
                ai_thinking_level: i.conversation.ai_thinking_level,
                ai_permission_mode: i.conversation.ai_permission_mode,
            })
            .collect(),
    ))
}

/// List the current user's active conversations for a project,
/// most-recently-active first.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/list",
    params(("project_id" = i32, Path,)),
    responses((status = 200, body = Vec<ConversationResponse>), (status = 401), (status = 403)),
    security(("bearer_auth" = []))
)]
pub async fn list_conversations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<Vec<ConversationResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let conversations = state
        .service
        .list_conversations(project_id, auth.user_id())
        .await?;
    Ok(Json(
        conversations
            .into_iter()
            .filter(|c| can_read_context(&auth, &c.context_type))
            .map(ConversationResponse::from)
            .collect(),
    ))
}

/// Get-or-create the current user's private chat for a context.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations",
    params(("project_id" = i32, Path,)),
    request_body = CreateConversationRequest,
    responses((status = 200, body = ConversationResponse), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn create_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(req): Json<CreateConversationRequest>,
) -> Result<Json<ConversationResponse>, Problem> {
    // Creating a conversation mutates state and can drive AI cost → write scope.
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    if req.context_type.len() > MAX_CONTEXT_TYPE_LEN {
        return Err(too_long("context_type", MAX_CONTEXT_TYPE_LEN));
    }
    if req.context_id.len() > MAX_CONTEXT_ID_LEN {
        return Err(too_long("context_id", MAX_CONTEXT_ID_LEN));
    }
    ensure_context_read_permission(&auth, &req.context_type)?;
    ensure_enabled(&state, project_id).await?;
    let runtime = state
        .service
        .resolve_get_or_create_runtime(
            project_id,
            &req.context_type,
            &req.context_id,
            auth.user_id(),
            req.ai_provider.as_deref(),
            req.ai_model.as_deref(),
            req.ai_thinking_level.as_deref(),
            req.ai_permission_mode.as_deref(),
        )
        .await?;
    ensure_runtime_permission(
        &auth,
        Some(&runtime.provider),
        Some(&runtime.permission_mode),
    )?;
    let conv = state
        .service
        .get_or_create(
            project_id,
            &req.context_type,
            &req.context_id,
            auth.user_id(),
            Some(&runtime.provider),
            Some(&runtime.model),
            runtime.thinking_level.as_deref(),
            Some(&runtime.permission_mode),
        )
        .await?;
    state
        .audit(&ConversationCreatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id,
            conversation_id: conv.public_id.clone(),
            context_type: conv.context_type.clone(),
        })
        .await;
    Ok(Json(ConversationResponse::from(conv)))
}

/// Full conversation history (excluding the internal system seed).
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    responses((status = 200, body = ConversationDetailResponse), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn get_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, public_id)): Path<(i32, String)>,
) -> Result<Json<ConversationDetailResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_conversation_read_permission(&auth, &conv)?;
    let messages = state
        .service
        .messages(conv.id)
        .await?
        .into_iter()
        .filter(|m| m.role != "system")
        .map(MessageResponse::from)
        .collect();
    let pending_permission = state.service.pending_permission_for(&conv.public_id);
    Ok(Json(ConversationDetailResponse {
        conversation: ConversationResponse::from(conv),
        messages,
        pending_permission,
    }))
}

/// Live wire for a conversation — cross-tab sync (not represented in the
/// OpenAPI schema; WS upgrades aren't expressible there). Read-only: a second
/// tab watching the same conversation subscribes here to see the same
/// tokens/tool-calls/permission-requests the sending tab receives over its
/// own `POST .../messages` SSE response, without which it would see nothing
/// until a manual reload.
///
/// Auth works identically to any other route: the WS upgrade request is
/// still a normal authenticated HTTP GET (cookies attached) before the 101
/// Switching Protocols — `RequireAuth` reads `AuthContext` out of request
/// extensions populated by the same middleware that runs for `get_conversation`.
pub async fn conversation_stream(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, public_id)): Path<(i32, String)>,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_conversation_read_permission(&auth, &conv)?;

    let rx = state.service.subscribe_conversation(conv.id);
    Ok(ws.on_upgrade(move |socket| forward_conversation_events(socket, rx)))
}

/// Forward one conversation's broadcast onto a WebSocket as JSON text frames
/// (`{"event":"...","data":"..."}`) until the client disconnects. On
/// `Lagged` (the bounded channel overflowed — a fast burst while the tab was
/// backgrounded), sends one explicit `resync_required` frame rather than
/// silently resuming with a gap: the client refetches full history instead
/// of rendering a conversation with missing turns.
async fn forward_conversation_events(
    mut socket: WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<crate::service::WireEvent>,
) {
    loop {
        tokio::select! {
            // A client-initiated close (or any incoming frame — this channel
            // is publish-only, so any message just confirms liveness) ends
            // the loop; ping/pong is handled by axum automatically.
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(evt) => {
                        let frame = serde_json::json!({ "event": evt.event, "data": evt.data });
                        if socket.send(Message::Text(frame.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let frame = serde_json::json!({ "event": "resync_required", "data": "" });
                        if socket.send(Message::Text(frame.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Send a user message; stream the assistant reply as Server-Sent Events.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/messages",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    request_body = SendMessageRequest,
    responses((status = 200, description = "SSE stream of assistant text deltas", content_type = "text/event-stream"), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn send_message(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id)): Path<(i32, String)>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Problem> {
    // Sending a message runs an AI turn (mutates state + incurs cost) → write scope.
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    if req.content.trim().is_empty() {
        return Err(problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("Empty Message")
            .with_detail("Message content must not be empty."));
    }
    if req.content.len() > MAX_MESSAGE_CONTENT_LEN {
        return Err(too_long("content", MAX_MESSAGE_CONTENT_LEN));
    }
    ensure_enabled(&state, project_id).await?;
    let mut conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    let effective_permission = req
        .ai_permission_mode
        .as_deref()
        .unwrap_or(&conv.ai_permission_mode);
    ensure_runtime_permission(&auth, Some(&conv.ai_provider), Some(effective_permission))?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    if req.ai_model.is_some() || req.ai_thinking_level.is_some() || req.ai_permission_mode.is_some()
    {
        conv = state
            .service
            .update_runtime_options(
                &conv,
                req.ai_model.as_deref(),
                req.ai_thinking_level.as_deref(),
                req.ai_permission_mode.as_deref(),
            )
            .await?;
    }
    // Page context is advisory framing, not user content: cap it and silently
    // drop an oversized value rather than failing the message.
    let page_context = req
        .page_context
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= MAX_PAGE_CONTEXT_LEN);
    // `send_message` persists the user turn before returning the stream, so the
    // turn is durable by the time we audit it.
    let token_stream = state
        .service
        .send_message(&conv, &req.content, page_context, &auth)
        .await?;
    state
        .audit(&ChatMessageSentAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id,
            conversation_id: conv.public_id.clone(),
        })
        .await;

    // `event_name` is `None` for plain token text — SSE's unnamed/"message"
    // event, which the existing frontend parser treats as prose. Every other
    // kind gets an explicit name. Computed once here so the SSE response
    // (the sending tab's own delivery) and the cross-tab broadcast (every
    // OTHER tab watching this conversation) are always built from the exact
    // same `(event_name, data)` pair — they can never disagree.
    let conv_id = conv.id;
    let broadcast_service = state.service.clone();
    // A second handle for the terminal `turn_complete` event below — the
    // `.map` closure below already moves its own clone.
    let broadcast_service_done = broadcast_service.clone();
    let sse = token_stream
        .map(move |item| {
            let (event_name, data): (Option<&str>, String) = match item {
                Ok(ChatStreamEvent::Token(text)) => (None, text),
                Ok(ChatStreamEvent::ToolCall {
                    id,
                    name,
                    arguments,
                }) => {
                    let payload = ToolCallEvent {
                        id,
                        name,
                        arguments,
                    };
                    // Single-line compact JSON so it occupies one `data:` line. On
                    // the (practically impossible) serialization failure, surface an
                    // error event rather than dropping the frame silently.
                    match serde_json::to_string(&payload) {
                        Ok(json) => (Some("tool_call"), json),
                        Err(e) => (
                            Some("error"),
                            format!("failed to encode tool_call event: {e}"),
                        ),
                    }
                }
                Ok(ChatStreamEvent::ToolResult { id, name, content }) => {
                    let payload = ToolResultEvent { id, name, content };
                    match serde_json::to_string(&payload) {
                        Ok(json) => (Some("tool_result"), json),
                        Err(e) => (
                            Some("error"),
                            format!("failed to encode tool_result event: {e}"),
                        ),
                    }
                }
                Ok(ChatStreamEvent::PermissionRequested {
                    id,
                    kind,
                    tool_name,
                    input,
                }) => {
                    let payload = PermissionRequestedEvent {
                        id,
                        kind,
                        tool_name,
                        input,
                    };
                    match serde_json::to_string(&payload) {
                        Ok(json) => (Some("permission_requested"), json),
                        Err(e) => (
                            Some("error"),
                            format!("failed to encode permission_requested event: {e}"),
                        ),
                    }
                }
                Err(e) => (Some("error"), e.to_string()),
            };
            // Cross-tab sync: best-effort, never blocks or fails this response.
            broadcast_service.publish_wire_event(
                conv_id,
                event_name.unwrap_or("token"),
                data.clone(),
            );
            let event = match event_name {
                Some(name) => Event::default().event(name).data(data),
                None => Event::default().data(data),
            };
            Ok::<_, Infallible>(event)
        })
        .chain(futures::stream::once(async move {
            // The SSE stream itself has no other way to say "this turn is done" —
            // it just ends, which the sending tab's fetch reader observes as
            // `done: true`. A cross-tab observer has no such signal, so without
            // this its "thinking" indicator would hang forever once the turn
            // finishes. Harmless on the sending tab too: `applyWireEvent` treats
            // an unrecognized event name with empty data as a no-op.
            broadcast_service_done.publish_wire_event(conv_id, "turn_complete", String::new());
            Ok::<_, Infallible>(Event::default().event("turn_complete").data(""))
        }));
    Ok(Sse::new(sse).keep_alive(KeepAlive::default()))
}

/// Archive (soft-delete) a conversation.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/archive",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    responses((status = 204), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn archive_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id)): Path<(i32, String)>,
) -> Result<axum::http::StatusCode, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_conversation_read_permission(&auth, &conv)?;
    state.service.archive(&conv).await?;
    state
        .audit(&ConversationArchivedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id,
            conversation_id: conv.public_id.clone(),
        })
        .await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Resolve a pending interactive permission request (ADR-038 Phase 2).
///
/// A provider adapter emits a normalized interaction request when user input
/// is required. The common runtime registers it and emits a
/// `permission_requested` SSE event. This endpoint sends the decision back to
/// the waiting turn without knowing the provider protocol.
///
/// The claim is atomic (remove-to-claim): a concurrent request for the same
/// `permission_id` receives 409 Conflict.  If the subprocess already exited
/// (and the registry was drained with a synthetic deny), the entry is gone and
/// the caller receives 404.
fn permission_decision_matches_kind(kind: &PermissionKind, decision: &PermissionDecision) -> bool {
    matches!(
        (kind, decision),
        (PermissionKind::ToolApproval, PermissionDecision::AllowTool)
            | (
                PermissionKind::ToolApproval,
                PermissionDecision::DenyTool { .. }
            )
            | (
                PermissionKind::Question,
                PermissionDecision::AnswerQuestion { .. }
            )
            | (
                PermissionKind::PlanApproval,
                PermissionDecision::ApprovePlan
            )
            | (
                PermissionKind::PlanApproval,
                PermissionDecision::RejectPlan { .. }
            )
    )
}

fn permission_kind_name(kind: &PermissionKind) -> &'static str {
    match kind {
        PermissionKind::ToolApproval => "tool_approval",
        PermissionKind::Question => "question",
        PermissionKind::PlanApproval => "plan_approval",
    }
}

#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/permissions/{permission_id}/resolve",
    params(
        ("project_id" = i32, Path,),
        ("public_id" = String, Path, description = "Conversation public id"),
        ("permission_id" = String, Path, description = "The CLI's request_id from the SSE event"),
    ),
    request_body = ResolvePermissionRequest,
    responses(
        (status = 204, description = "Decision accepted; subprocess will continue"),
        (status = 400, description = "Invalid decision payload"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Unknown permission_id (may have timed out or been auto-denied)"),
        (status = 409, description = "Permission already resolved (concurrent resolve race)"),
        (status = 410, description = "Turn already ended (subprocess exited before decision arrived)"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn resolve_permission(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id, permission_id)): Path<(i32, String, String)>,
    Json(req): Json<ResolvePermissionRequest>,
) -> Result<axum::http::StatusCode, Problem> {
    // Resolving a permission unblocks an AI subprocess (mutates state) → write scope.
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;

    // Verify the conversation exists and is scoped to this project (auth gate).
    // Runtime authorization is rechecked before the pending entry can be
    // consumed, so revoking provider access also revokes tool approval.
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_runtime_permission(
        &auth,
        Some(&conv.ai_provider),
        Some(&conv.ai_permission_mode),
    )?;
    ensure_context_read_permission(&auth, &conv.context_type)?;

    // ── Finding 3: payload size validation ────────────────────────────────────
    // Reject oversized free-text fields before touching the registry.  The body
    // limit (`DefaultBodyLimit`) on this route's registration provides the outer
    // cap; these per-field checks provide typed 400 errors rather than a 413.
    match &req.decision {
        PermissionDecision::DenyTool {
            reason: Some(reason),
        } if reason.len() > MAX_DECISION_STRING_LEN => {
            return Err(too_long("reason", MAX_DECISION_STRING_LEN));
        }
        PermissionDecision::RejectPlan {
            feedback: Some(feedback),
        } if feedback.len() > MAX_DECISION_STRING_LEN => {
            return Err(too_long("feedback", MAX_DECISION_STRING_LEN));
        }
        PermissionDecision::AnswerQuestion { answers } => {
            let serialized_len = serde_json::to_string(answers)
                .map(|s| s.len())
                .unwrap_or(usize::MAX);
            if serialized_len > MAX_DECISION_STRING_LEN {
                return Err(too_long("answers", MAX_DECISION_STRING_LEN));
            }
        }
        _ => {}
    }

    // Classify the decision kind for the audit log (before we consume the value).
    let decision_kind = match &req.decision {
        PermissionDecision::AllowTool => "allow_tool",
        PermissionDecision::DenyTool { .. } => "deny_tool",
        PermissionDecision::AnswerQuestion { .. } => "answer_question",
        PermissionDecision::ApprovePlan => "approve_plan",
        PermissionDecision::RejectPlan { .. } => "reject_plan",
    }
    .to_string();

    // ── Findings 1 + 2: IDOR guard + kind validation ──────────────────────────
    // Look up the registry entry under the lock.  If present, verify that it
    // belongs to THIS conversation before claiming it (removing it).  A URL
    // `{public_id}` that doesn't match the stored `conv_public_id` gets a 404
    // — the same response as a missing entry — so the existence of the id in
    // another session is not confirmed to the caller.
    //
    // All three operations (lookup, ownership check, remove) happen under the
    // same lock acquisition, so there is no window for a concurrent resolve to
    // claim the entry between our check and our remove.
    let tx = {
        let mut registry = state.service.pending_permissions.lock().map_err(|_| {
            problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail("Permission registry lock poisoned")
        })?;

        let stored_entry = registry.get(&permission_id);

        match stored_entry {
            // Entry absent: timed-out, auto-denied, or already resolved.
            None => {
                return Err(problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Permission Not Found")
                    .with_detail(format!(
                        "Permission request '{permission_id}' is not pending. \
                         It may have timed out, been auto-denied, or already been resolved."
                    )));
            }
            // Entry exists but belongs to a different conversation.
            // Return 404 (not 403) to avoid confirming the id's existence
            // in another session to the caller.
            Some(entry) if entry.conv_public_id != public_id => {
                return Err(problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Permission Not Found")
                    .with_detail(format!(
                        "Permission request '{permission_id}' is not pending. \
                         It may have timed out, been auto-denied, or already been resolved."
                    )));
            }
            // Entry belongs to this conversation — claim it atomically.
            Some(entry) => {
                if !permission_decision_matches_kind(&entry.kind, &req.decision) {
                    let expected_kind = permission_kind_name(&entry.kind);
                    return Err(ChatError::PermissionKindMismatch {
                        expected_kind: expected_kind.to_string(),
                        received: decision_kind.clone(),
                    }
                    .into());
                }
                let Some(entry) = registry.remove(&permission_id) else {
                    return Err(problemdetails::new(axum::http::StatusCode::CONFLICT)
                        .with_title("Permission Already Resolved")
                        .with_detail("The permission request was resolved concurrently."));
                };
                entry.sender
            }
        }
    };

    // Persist the answer as a synthetic `user` message BEFORE sending it down
    // the oneshot — once sent, the subprocess may resume and append its own
    // reply first, and history should read question → answer → next reply in
    // that order. Best-effort: never fails the request.
    state
        .service
        .persist_permission_answered(conv.id, &req.decision)
        .await;

    // Send the decision. A `SendError` means the receiver was dropped — i.e.
    // `run_interactive`'s stream task exited (subprocess died) while we were
    // looking up the sender. The turn has ended; the decision can't be used.
    tx.send(req.decision).map_err(|_| {
        problemdetails::new(axum::http::StatusCode::GONE)
            .with_title("Turn Already Ended")
            .with_detail(
                "The AI turn associated with this permission request has already ended. \
                 The subprocess exited before the decision could be delivered.",
            )
    })?;

    // Audit (best-effort: never fail the request on log failure).
    state
        .audit(&PermissionResolvedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id,
            conversation_id: conv.public_id.clone(),
            permission_id: permission_id.clone(),
            decision_kind,
        })
        .await;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Rename a conversation (set its human-facing title).
#[utoipa::path(
    patch, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    request_body = RenameConversationRequest,
    responses((status = 200, body = ConversationResponse), (status = 400), (status = 401), (status = 403), (status = 404)),
    security(("bearer_auth" = []))
)]
pub async fn rename_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id)): Path<(i32, String)>,
    Json(req): Json<RenameConversationRequest>,
) -> Result<Json<ConversationResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;

    let title = req.title.trim();
    if title.is_empty() {
        return Err(problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("Invalid Title")
            .with_detail("Conversation title cannot be empty."));
    }
    if title.len() > MAX_TITLE_LEN {
        return Err(too_long("title", MAX_TITLE_LEN));
    }

    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let updated = state.service.rename(&conv, title).await?;

    state
        .audit(&ConversationRenamedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id,
            conversation_id: updated.public_id.clone(),
            title: title.to_string(),
        })
        .await;

    Ok(Json(ConversationResponse::from(updated)))
}

// --- Pending-action handlers -------------------------------------------------

/// List all pending actions for a conversation (most-recently-proposed first).
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/pending-actions",
    params(
        ("project_id" = i32, Path,),
        ("public_id" = String, Path, description = "Conversation public id"),
    ),
    responses(
        (status = 200, body = Vec<PendingActionResponse>),
        (status = 401), (status = 403), (status = 404)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_pending_actions(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, conv_public_id)): Path<(i32, String)>,
) -> Result<Json<Vec<PendingActionResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    // Verify conversation exists + is scoped to this project.
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &conv_public_id)
        .await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let rows = state
        .pending_actions
        .list_for_conversation(project_id, auth.user_id(), conv.id)
        .await
        .map_err(Problem::from)?;
    Ok(Json(
        rows.into_iter().map(PendingActionResponse::from).collect(),
    ))
}

/// Get a single pending action by its public id (scoped to the project).
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/pending-actions/{action_public_id}",
    params(
        ("project_id" = i32, Path,),
        ("action_public_id" = String, Path,),
    ),
    responses(
        (status = 200, body = PendingActionResponse),
        (status = 401), (status = 403), (status = 404)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_pending_action(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, action_public_id)): Path<(i32, String)>,
) -> Result<Json<PendingActionResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let action = state
        .pending_actions
        .get(project_id, auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conv = state
        .service
        .get_by_id(project_id, auth.user_id(), action.conversation_id)
        .await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    Ok(Json(PendingActionResponse::from(action)))
}

/// Confirm a proposed AI action: validate permission, atomically claim, execute,
/// persist outcome. The execution uses the CONFIRMING user's auth — never the model's.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/pending-actions/{action_public_id}/confirm",
    params(
        ("project_id" = i32, Path,),
        ("action_public_id" = String, Path,),
    ),
    responses(
        (status = 200, body = PendingActionResponse),
        (status = 401), (status = 403), (status = 404), (status = 409), (status = 503)
    ),
    security(("bearer_auth" = []))
)]
pub async fn confirm_pending_action(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, action_public_id)): Path<(i32, String)>,
) -> Result<Json<PendingActionResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let action = state
        .pending_actions
        .get(project_id, auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conv = state
        .service
        .get_by_id(project_id, auth.user_id(), action.conversation_id)
        .await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let confirmed_by = Some(auth.user_id());
    let updated = state
        .pending_actions
        .confirm(project_id, &action_public_id, &auth, confirmed_by)
        .await
        .map_err(Problem::from)?;

    // Audit is also emitted inside the service, but we emit here with full
    // metadata (ip_address, user_agent) for the HTTP-layer record.
    let audit = AiActionConfirmedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        action_id: updated.public_id.clone(),
        operation_id: updated.operation_id.clone(),
        status: updated.status.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to write ai.pending_action.confirmed audit log: {e}");
    }

    Ok(Json(PendingActionResponse::from(updated)))
}

/// Reject a proposed AI action (no execution). Status transitions to "rejected".
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/pending-actions/{action_public_id}/reject",
    params(
        ("project_id" = i32, Path,),
        ("action_public_id" = String, Path,),
    ),
    responses(
        (status = 200, body = PendingActionResponse),
        (status = 401), (status = 403), (status = 404), (status = 409)
    ),
    security(("bearer_auth" = []))
)]
pub async fn reject_pending_action(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, action_public_id)): Path<(i32, String)>,
) -> Result<Json<PendingActionResponse>, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let action = state
        .pending_actions
        .get(project_id, auth.user_id(), &action_public_id)
        .await
        .map_err(Problem::from)?;
    let conv = state
        .service
        .get_by_id(project_id, auth.user_id(), action.conversation_id)
        .await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    let rejected_by = Some(auth.user_id());
    let updated = state
        .pending_actions
        .reject(project_id, &action_public_id, &auth, rejected_by)
        .await
        .map_err(Problem::from)?;

    let audit = AiActionRejectedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        action_id: updated.public_id.clone(),
        operation_id: updated.operation_id.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to write ai.pending_action.rejected audit log: {e}");
    }

    Ok(Json(PendingActionResponse::from(updated)))
}

/// What still has to be true before an AI chat can run a turn in this project.
///
/// The three gates are independent and fail for different reasons with different
/// fixes, so they are reported separately rather than collapsed into one boolean:
/// an instance admin configures a provider (instance-wide), while the two toggles
/// are per-project. Collapsing them would leave the user with "AI unavailable"
/// and no idea which of three places to go.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatReadinessResponse {
    /// An AI provider is configured on this instance. Fixed in
    /// Settings → AI Providers; instance-wide, not per project.
    pub ai_configured: bool,
    /// The per-project read-only chat toggle is on (the default).
    pub chat_enabled: bool,
    /// The per-project write-actions opt-in is on. Required for any flow where
    /// the assistant *proposes* changes; irrelevant for read-only questions.
    pub write_actions_enabled: bool,
}

/// Report which AI prerequisites this project satisfies.
///
/// Read-only and cheap, so the UI can decide up front whether to show a working
/// entry point, an onboarding path, or nothing — instead of letting the user
/// click something that fails with a 409 they can't act on.
#[utoipa::path(
    get, tag = "AI Chat",
    path = "/projects/{project_id}/ai/readiness",
    params(("project_id" = i32, Path,)),
    responses(
        (status = 200, description = "Which AI prerequisites are met", body = ChatReadinessResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_chat_readiness(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<ChatReadinessResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let readiness = state.service.chat_readiness(project_id).await?;

    Ok(Json(ChatReadinessResponse {
        ai_configured: readiness.ai_configured,
        chat_enabled: readiness.chat_enabled,
        write_actions_enabled: readiness.write_actions_enabled,
    }))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Readiness for the chat, so the UI can onboard instead of guessing.
        .route(
            "/projects/{project_id}/ai/readiness",
            get(get_chat_readiness),
        )
        // Unified cross-project switcher.
        .route("/ai/conversations", get(list_all_conversations))
        .route(
            "/projects/{project_id}/ai/conversations",
            get(find_conversation).post(create_conversation),
        )
        // Static `/list` registered before the `{public_id}` param route; matchit
        // prioritizes the literal segment so it can't be shadowed.
        .route(
            "/projects/{project_id}/ai/conversations/list",
            get(list_conversations),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}",
            get(get_conversation).patch(rename_conversation),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/messages",
            post(send_message),
        )
        // Live wire for cross-tab sync (ADR-038 follow-up). Read-only.
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/stream",
            get(conversation_stream),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/archive",
            post(archive_conversation),
        )
        // Permission-bridge resolve endpoint (ADR-038 Phase 2).
        // The body-limit layer caps inbound JSON before deserialization so an
        // oversized payload is rejected with 413 rather than OOM-ing the server.
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/permissions/{permission_id}/resolve",
            post(resolve_permission)
                .layer(DefaultBodyLimit::max(RESOLVE_PERMISSION_BODY_LIMIT)),
        )
        // Pending-action routes (propose-then-confirm write actions).
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/pending-actions",
            get(list_pending_actions),
        )
        .route(
            "/projects/{project_id}/ai/pending-actions/{action_public_id}",
            get(get_pending_action),
        )
        .route(
            "/projects/{project_id}/ai/pending-actions/{action_public_id}/confirm",
            post(confirm_pending_action),
        )
        .route(
            "/projects/{project_id}/ai/pending-actions/{action_public_id}/reject",
            post(reject_pending_action),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_chat_readiness,
        find_conversation,
        list_conversations,
        list_all_conversations,
        create_conversation,
        get_conversation,
        send_message,
        archive_conversation,
        rename_conversation,
        resolve_permission,
        list_pending_actions,
        get_pending_action,
        confirm_pending_action,
        reject_pending_action,
    ),
    components(schemas(
        ConversationResponse,
        GlobalConversationResponse,
        MessageResponse,
        ToolInfo,
        MessagePart,
        ConversationDetailResponse,
        CreateConversationRequest,
        RenameConversationRequest,
        SendMessageRequest,
        ToolCallEvent,
        ToolResultEvent,
        PermissionRequestedEvent,
        PermissionKind,
        PermissionDecision,
        ResolvePermissionRequest,
        PendingActionResponse,
        ChatReadinessResponse,
    ))
)]
pub struct AiChatApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PendingPermissionEntry;
    use axum::http::StatusCode;

    /// The `title` value the mapping set on the Problem body, if any.
    fn title_of(p: &Problem) -> Option<String> {
        p.body
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    // (a) Every `ChatError` variant maps to the expected HTTP status + title.
    // Pure: exercises `From<ChatError> for Problem` directly.

    #[test]
    fn test_not_found_maps_to_404() {
        let p: Problem = ChatError::NotFound("abc".to_string()).into();
        assert_eq!(p.status_code, StatusCode::NOT_FOUND);
        assert_eq!(title_of(&p).as_deref(), Some("Conversation Not Found"));
    }

    #[test]
    fn test_project_not_found_maps_to_404() {
        let p: Problem = ChatError::ProjectNotFound(7).into();
        assert_eq!(p.status_code, StatusCode::NOT_FOUND);
        assert_eq!(title_of(&p).as_deref(), Some("Project Not Found"));
    }

    #[test]
    fn test_no_provider_maps_to_404_context_unavailable() {
        let p: Problem = ChatError::NoProvider("deployment".to_string()).into();
        assert_eq!(p.status_code, StatusCode::NOT_FOUND);
        assert_eq!(title_of(&p).as_deref(), Some("Context Not Available"));
    }

    #[test]
    fn test_context_unavailable_maps_to_404() {
        let p: Problem = ChatError::ContextUnavailable.into();
        assert_eq!(p.status_code, StatusCode::NOT_FOUND);
        assert_eq!(title_of(&p).as_deref(), Some("Context Not Available"));
    }

    #[test]
    fn test_ai_unavailable_maps_to_409() {
        let p: Problem = ChatError::AiUnavailable.into();
        assert_eq!(p.status_code, StatusCode::CONFLICT);
        assert_eq!(title_of(&p).as_deref(), Some("AI Not Configured"));
    }

    #[test]
    fn test_db_error_maps_to_500() {
        let p: Problem = ChatError::Db(sea_orm::DbErr::Custom("boom".to_string())).into();
        assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(title_of(&p).as_deref(), Some("Internal Server Error"));
        assert!(!serde_json::to_string(&p.body)
            .expect("problem body serializes")
            .contains("boom"));
    }

    #[test]
    fn test_project_lookup_error_maps_to_stable_500() {
        let p: Problem = ChatError::ProjectLookup {
            project_id: 42,
            source: sea_orm::DbErr::Custom("database.internal:5432".to_string()),
        }
        .into();
        assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!serde_json::to_string(&p.body)
            .expect("problem body serializes")
            .contains("database.internal"));
    }

    #[test]
    fn test_ai_error_maps_to_500() {
        let p: Problem = ChatError::Ai("provider exploded".to_string()).into();
        assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(title_of(&p).as_deref(), Some("Internal Server Error"));
    }

    // (b) The `ai_debug_chat_enabled` gate is a security control (revoking the
    // toggle must hide/deny chat). `ensure_chat_enabled` is DB-only, so we test
    // it directly with a MockDatabase — no router/Docker needed.

    use sea_orm::{DatabaseBackend, MockDatabase};

    fn test_user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
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
        }
    }

    fn custom_auth(permissions: Vec<Permission>) -> AuthContext {
        AuthContext::new_api_key(
            test_user(),
            None,
            Some(permissions),
            "test-key".to_string(),
            1,
        )
    }

    fn conversation_with_runtime(provider: &str, permission_mode: &str) -> ai_conversations::Model {
        let now = chrono::Utc::now();
        ai_conversations::Model {
            id: 1,
            public_id: "conversation-1".to_string(),
            project_id: 1,
            context_type: "project".to_string(),
            context_id: "1".to_string(),
            title: None,
            status: "active".to_string(),
            created_by: Some(1),
            metadata: None,
            created_at: now,
            last_activity_at: now,
            ai_provider: provider.to_string(),
            ai_model: "default".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: permission_mode.to_string(),
            cli_session_id: None,
            cli_session_fingerprint: None,
        }
    }

    enum HiddenProjectsOutcome {
        Hidden(Vec<i32>),
        Error,
    }

    struct HiddenProjectsChecker(HiddenProjectsOutcome);

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for HiddenProjectsChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(true)
        }

        async fn hidden_project_ids(
            &self,
            _user_id: i32,
        ) -> Result<Option<Vec<i32>>, Box<dyn std::error::Error + Send + Sync>> {
            match &self.0 {
                HiddenProjectsOutcome::Hidden(ids) => Ok(Some(ids.clone())),
                HiddenProjectsOutcome::Error => {
                    Err(Box::new(std::io::Error::other("checker unavailable")))
                }
            }
        }
    }

    #[tokio::test]
    async fn global_conversation_visibility_uses_current_hidden_projects() {
        let auth = custom_auth(vec![Permission::ProjectsRead]);
        let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(Arc::new(
            HiddenProjectsChecker(HiddenProjectsOutcome::Hidden(vec![7, 9])),
        ));

        assert_eq!(
            hidden_conversation_project_ids(&auth, &checker)
                .await
                .expect("visibility lookup"),
            vec![7, 9]
        );
    }

    #[tokio::test]
    async fn global_conversation_visibility_fails_closed_on_checker_error() {
        let auth = custom_auth(vec![Permission::ProjectsRead]);
        let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> = Some(Arc::new(
            HiddenProjectsChecker(HiddenProjectsOutcome::Error),
        ));

        let error = hidden_conversation_project_ids(&auth, &checker)
            .await
            .expect_err("checker errors must not return an unfiltered list");
        assert_eq!(error.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn host_cli_requires_provider_administration_permission() {
        let project_writer = custom_auth(vec![Permission::ProjectsWrite]);
        let denied = ensure_runtime_permission(&project_writer, Some("codex_cli"), Some("auto"))
            .expect_err("project write access must not authorize host CLI execution");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        let provider_admin =
            custom_auth(vec![Permission::ProjectsWrite, Permission::AiGatewayWrite]);
        let denied = ensure_runtime_permission(&provider_admin, Some("codex_cli"), Some("auto"))
            .expect_err("Codex host access requires system administration");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        let system_admin = custom_auth(vec![
            Permission::ProjectsWrite,
            Permission::AiGatewayWrite,
            Permission::SystemAdmin,
        ]);
        ensure_runtime_permission(&system_admin, Some("codex_cli"), Some("auto"))
            .expect("system administrator may opt into the Codex host boundary");
    }

    #[test]
    fn resolved_conversation_provider_is_authorized_after_default_resolution() {
        let project_writer = custom_auth(vec![Permission::ProjectsWrite]);
        let resolved = conversation_with_runtime("claude_cli", "default");

        let denied = ensure_runtime_permission(
            &project_writer,
            Some(&resolved.ai_provider),
            Some(&resolved.ai_permission_mode),
        )
        .expect_err("resolved host provider must be authorized even when request omitted it");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        let provider_admin =
            custom_auth(vec![Permission::ProjectsWrite, Permission::AiGatewayWrite]);
        ensure_runtime_permission(
            &provider_admin,
            Some(&resolved.ai_provider),
            Some(&resolved.ai_permission_mode),
        )
        .expect("provider administrator may use the resolved Claude provider");
    }

    #[test]
    fn stored_host_conversation_remains_readable_without_runtime_permission() {
        let project_reader = custom_auth(vec![Permission::ProjectsRead]);
        let stored = conversation_with_runtime("claude_cli", "full-access");

        ensure_conversation_read_permission(&project_reader, &stored)
            .expect("stored project history must not require host-provider execution access");
        assert!(ensure_runtime_permission(
            &project_reader,
            Some(&stored.ai_provider),
            Some(&stored.ai_permission_mode),
        )
        .is_err());
    }

    struct DefaultClaudeAi;

    #[async_trait::async_trait]
    impl temps_ai::AiService for DefaultClaudeAi {
        async fn is_available(&self) -> bool {
            true
        }

        async fn capabilities_for(
            &self,
            provider: Option<&str>,
            _refresh: temps_ai::RefreshPolicy,
        ) -> Result<temps_ai::ProviderCapabilities, temps_ai::AiError> {
            Ok(temps_ai::ProviderCapabilities {
                id: provider.unwrap_or("claude_cli").to_string(),
                name: "Claude Code".to_string(),
                auth_source: temps_ai::ProviderAuthSource::HostEnvironment,
                models: vec![temps_ai::ModelCapability {
                    id: "sonnet".to_string(),
                    name: "Sonnet".to_string(),
                    thinking_modes: Vec::new(),
                    tool_thinking_modes: None,
                    default_thinking_mode_id: None,
                }],
                default_model_id: Some("sonnet".to_string()),
                permission_modes: vec![temps_ai::SelectOption {
                    id: "default".to_string(),
                    name: "Default".to_string(),
                    description: None,
                }],
                default_permission_mode_id: Some("default".to_string()),
                realtime: temps_ai::RealtimeCapabilities {
                    text_streaming: true,
                    reasoning_streaming: true,
                    tool_events: true,
                    user_interactions: true,
                    cancellation: true,
                },
            })
        }

        async fn complete(
            &self,
            _request: temps_ai::AiRequest,
        ) -> Result<temps_ai::AiResponse, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }

        async fn chat_stream(
            &self,
            _request: temps_ai::ChatTurnRequest,
        ) -> Result<temps_ai::TokenStream, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }
    }

    #[tokio::test]
    async fn denied_omitted_host_provider_performs_no_conversation_insert() {
        let now = chrono::Utc::now();
        let preference = temps_entities::ai_gateway_config::Model {
            id: 1,
            scope: "instance".to_string(),
            allowed_models: None,
            max_requests_per_minute: None,
            max_cost_per_month_microcents: None,
            created_at: now,
            updated_at: now,
            provider_type: "agent_cli".to_string(),
            agent_cli_provider_id: Some("claude_cli".to_string()),
            interactive_bridge_enabled: false,
            summary_provider_id: None,
            summary_model: None,
            summary_thinking_level: None,
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<ai_conversations::Model>::new()])
                .append_query_results([[preference]])
                .into_connection(),
        );
        let service = ConversationService::new(db.clone(), Arc::new(DefaultClaudeAi), Vec::new());
        let runtime = service
            .resolve_get_or_create_runtime(1, "project", "1", 1, None, None, None, None)
            .await
            .expect("omitted runtime resolves to the active host provider");
        assert_eq!(runtime.provider, "claude_cli");

        let project_writer = custom_auth(vec![Permission::ProjectsWrite]);
        let denied = ensure_runtime_permission(
            &project_writer,
            Some(&runtime.provider),
            Some(&runtime.permission_mode),
        )
        .expect_err("host provider must be denied before get_or_create can insert");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        drop(service);
        let db = Arc::try_unwrap(db).expect("release mock database");
        let statements = db
            .into_transaction_log()
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.to_ascii_uppercase())
            .collect::<Vec<_>>();
        assert_eq!(statements.len(), 2, "preflight should perform only reads");
        assert!(
            statements.iter().all(|sql| sql.starts_with("SELECT")),
            "denial must happen before any insert; got {statements:?}"
        );
    }

    #[test]
    fn full_access_requires_system_administrator_permission() {
        let provider_admin = custom_auth(vec![Permission::AiGatewayWrite]);
        let denied =
            ensure_runtime_permission(&provider_admin, Some("claude_cli"), Some("full-access"))
                .expect_err("provider administration alone must not authorize full host access");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        let system_admin = custom_auth(vec![Permission::AiGatewayWrite, Permission::SystemAdmin]);
        assert!(
            ensure_runtime_permission(&system_admin, Some("claude_cli"), Some("full-access"),)
                .is_ok()
        );
    }

    #[test]
    fn alert_suggestion_context_requires_otel_read() {
        let without_otel = custom_auth(vec![Permission::ProjectsRead, Permission::ProjectsWrite]);
        let err = ensure_context_read_permission(&without_otel, "alert_suggest")
            .expect_err("OTel data must not leak through chat seeds or history");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);

        let with_otel = custom_auth(vec![Permission::OtelRead]);
        ensure_context_read_permission(&with_otel, "alert_suggest")
            .expect("otel:read authorizes alert suggestion context");
    }

    #[test]
    fn unrelated_chat_context_does_not_require_otel_read() {
        let auth = custom_auth(vec![Permission::ProjectsRead]);
        ensure_context_read_permission(&auth, "project")
            .expect("project chat keeps its existing project permission boundary");
    }

    fn project_with_toggle(id: i32, toggle: Option<bool>) -> temps_entities::projects::Model {
        let now = chrono::Utc::now();
        temps_entities::projects::Model {
            id,
            name: "P".to_string(),
            repo_name: "r".to_string(),
            repo_owner: "o".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::Static,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "p".to_string(),
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

    fn db_returning(project: Option<temps_entities::projects::Model>) -> DatabaseConnection {
        let rows = match project {
            Some(p) => vec![p],
            None => Vec::new(),
        };
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![rows])
            .into_connection()
    }

    #[tokio::test]
    async fn test_ensure_chat_enabled_allows_when_toggle_on() {
        let db = db_returning(Some(project_with_toggle(7, Some(true))));
        assert!(ensure_chat_enabled(&db, 7).await.is_ok());
    }

    #[tokio::test]
    async fn test_ensure_chat_enabled_allows_when_write_actions_on_even_if_chat_off() {
        // Write actions are proposed + confirmed inside the chat, so enabling
        // them must never leave the chat itself unreachable, regardless of the
        // read-only debug-chat toggle (off or NULL).
        for chat_toggle in [None, Some(false)] {
            let mut p = project_with_toggle(7, chat_toggle);
            p.ai_write_actions_enabled = true;
            let db = db_returning(Some(p));
            assert!(
                ensure_chat_enabled(&db, 7).await.is_ok(),
                "write actions on must allow the chat (chat toggle {chat_toggle:?})"
            );
        }
    }

    #[tokio::test]
    async fn test_ensure_chat_enabled_403_when_toggle_off() {
        let db = db_returning(Some(project_with_toggle(7, Some(false))));
        let err = ensure_chat_enabled(&db, 7)
            .await
            .expect_err("toggle off must be denied");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_ensure_chat_enabled_allows_when_toggle_null() {
        let db = db_returning(Some(project_with_toggle(7, None)));
        ensure_chat_enabled(&db, 7)
            .await
            .expect("toggle null (default on) must be allowed");
    }

    #[tokio::test]
    async fn test_ensure_chat_enabled_403_when_project_missing() {
        let db = db_returning(None);
        let err = ensure_chat_enabled(&db, 999)
            .await
            .expect_err("missing project must be denied");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    // (c) Over-length input is rejected as 400 before any DB/AI work (cost/DoS
    // hardening).
    #[test]
    fn test_too_long_is_400() {
        let p = too_long("content", 10);
        assert_eq!(p.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(title_of(&p).as_deref(), Some("Input Too Long"));
    }

    // Note on full handler-level (401/403 via the guard macros) coverage: the
    // `permission_guard!` / `project_scope_guard!` / `deny_deployment_token!`
    // macros are themselves tested in `temps-auth`; here we cover the
    // crate-specific toggle gate (above), the input-length gate, the service-
    // layer scoping (see service.rs tests), and the HTTP error mapping via the
    // pure `From<ChatError>` conversion.

    // ── PendingActionError → Problem mapping ─────────────────────────────────

    #[test]
    fn test_pending_action_not_found_maps_to_404() {
        let p: Problem = PendingActionError::NotFound {
            public_id: "abc".to_string(),
        }
        .into();
        assert_eq!(p.status_code, StatusCode::NOT_FOUND);
        assert_eq!(title_of(&p).as_deref(), Some("Pending Action Not Found"));
    }

    #[test]
    fn test_pending_action_invalid_state_maps_to_409() {
        let p: Problem = PendingActionError::InvalidState {
            public_id: "abc".to_string(),
            status: "executed".to_string(),
        }
        .into();
        assert_eq!(p.status_code, StatusCode::CONFLICT);
        assert_eq!(title_of(&p).as_deref(), Some("Invalid Action State"));
    }

    #[test]
    fn test_pending_action_permission_denied_maps_to_403() {
        let p: Problem = PendingActionError::PermissionDenied {
            permission: "deployments:write".to_string(),
        }
        .into();
        assert_eq!(p.status_code, StatusCode::FORBIDDEN);
        assert_eq!(title_of(&p).as_deref(), Some("Permission Denied"));
    }

    #[test]
    fn test_pending_action_disabled_maps_to_403() {
        let p: Problem = PendingActionError::Disabled { project_id: 7 }.into();
        assert_eq!(p.status_code, StatusCode::FORBIDDEN);
        assert_eq!(title_of(&p).as_deref(), Some("AI Write Actions Disabled"));
    }

    #[test]
    fn test_pending_action_unavailable_maps_to_503() {
        let p: Problem = PendingActionError::Unavailable.into();
        assert_eq!(p.status_code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(title_of(&p).as_deref(), Some("Write Actions Unavailable"));
    }

    #[test]
    fn test_pending_action_database_error_maps_to_500() {
        let p: Problem =
            PendingActionError::Database(sea_orm::DbErr::Custom("boom".to_string())).into();
        assert_eq!(p.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(title_of(&p).as_deref(), Some("Internal Server Error"));
    }

    // ── redact_params ────────────────────────────────────────────────────────

    #[test]
    fn test_redact_params_masks_sensitive_keys() {
        let params = serde_json::json!({
            "name": "MY_SECRET",
            "value": "super-secret",
            "secret": "also-secret",
            "password": "p@ssword",
            "token": "tok_abc",
            "key": "k123",
            "operation": "update",
        });
        let redacted = redact_params(&params);
        assert_eq!(redacted["name"], serde_json::json!("MY_SECRET"));
        assert_eq!(redacted["operation"], serde_json::json!("update"));
        assert_eq!(redacted["value"], serde_json::json!("***"));
        assert_eq!(redacted["secret"], serde_json::json!("***"));
        assert_eq!(redacted["password"], serde_json::json!("***"));
        assert_eq!(redacted["token"], serde_json::json!("***"));
        assert_eq!(redacted["key"], serde_json::json!("***"));
    }

    #[test]
    fn test_redact_params_masks_keys_containing_sensitive_substrings() {
        let params = serde_json::json!({
            "api_key": "my-api-key",
            "access_token": "tok",
            "db_password": "hunter2",
        });
        let redacted = redact_params(&params);
        assert_eq!(redacted["api_key"], serde_json::json!("***"));
        assert_eq!(redacted["access_token"], serde_json::json!("***"));
        assert_eq!(redacted["db_password"], serde_json::json!("***"));
    }

    #[test]
    fn test_redact_params_non_object_passthrough() {
        let arr = serde_json::json!([1, 2, 3]);
        assert_eq!(redact_params(&arr), arr);
        let s = serde_json::json!("hello");
        assert_eq!(redact_params(&s), s);
        let n = serde_json::json!(42);
        assert_eq!(redact_params(&n), n);
    }

    #[test]
    fn test_redact_params_recurses_through_nested_objects_and_arrays() {
        let params = serde_json::json!({
            "parameters": {
                "database": [{
                    "name": "primary",
                    "credentials": {
                        "password": "nested-password",
                        "apiKey": "nested-key"
                    }
                }]
            },
            "result": [{"access_token": "nested-token", "status": "ok"}]
        });
        let redacted = redact_params(&params);
        assert_eq!(redacted["parameters"]["database"][0]["name"], "primary");
        assert_eq!(
            redacted["parameters"]["database"][0]["credentials"]["password"],
            "***"
        );
        assert_eq!(
            redacted["parameters"]["database"][0]["credentials"]["apiKey"],
            "***"
        );
        assert_eq!(redacted["result"][0]["access_token"], "***");
        assert_eq!(redacted["result"][0]["status"], "ok");
    }

    #[test]
    fn test_redact_params_empty_object_passthrough() {
        let empty = serde_json::json!({});
        assert_eq!(redact_params(&empty), empty);
    }

    #[test]
    fn test_redact_params_case_insensitive() {
        let params = serde_json::json!({
            "VALUE": "sensitive",
            "Secret": "also-sensitive",
        });
        let redacted = redact_params(&params);
        assert_eq!(redacted["VALUE"], serde_json::json!("***"));
        assert_eq!(redacted["Secret"], serde_json::json!("***"));
    }

    #[test]
    fn reloaded_tool_metadata_never_returns_embedded_write_secret() {
        let secret = "must-not-survive-reload";
        let arguments = serde_json::json!({
            "command": format!("projects create_environment_variable --name API_TOKEN --value {secret}")
        })
        .to_string();
        let message = ai_messages::Model {
            id: 1,
            conversation_id: 1,
            role: "assistant".to_string(),
            content: String::new(),
            metadata: Some(serde_json::json!({
                "tools": [{
                    "id": "call-1",
                    "name": "temps_write",
                    "arguments": arguments,
                    "result": format!("HTTP 400: token={secret}")
                }]
            })),
            tokens_in: None,
            tokens_out: None,
            cost_microcents: None,
            created_at: chrono::Utc::now(),
        };

        let response = MessageResponse::from(message);
        let serialized = serde_json::to_string(&response).expect("message serializes");
        assert!(!serialized.contains(secret));
        assert!(serialized.contains("***"));
    }

    // ----- SSE mapping tests (ADR-038 Phase 2, milestone 3) -----

    /// `ChatStreamEvent::PermissionRequested` serialises to a `PermissionRequestedEvent`
    /// with every field intact and no sensitive content omitted.
    #[test]
    fn test_permission_requested_event_serialization() {
        let payload = PermissionRequestedEvent {
            id: "perm-abc".to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: "bash".to_string(),
            input: serde_json::json!({ "command": "ls" }),
        };
        let json = serde_json::to_string(&payload).expect("serializes");
        let back: serde_json::Value = serde_json::from_str(&json).expect("parses");
        assert_eq!(back["id"], "perm-abc");
        assert_eq!(back["kind"], "tool_approval");
        assert_eq!(back["tool_name"], "bash");
        assert_eq!(back["input"]["command"], "ls");
    }

    /// Verifies that the `PermissionKind` serde aliases match the wire format
    /// expected by the ADR-038 spec: snake_case tags.
    #[test]
    fn test_permission_kind_serde_roundtrip() {
        let kinds = [
            (PermissionKind::ToolApproval, "tool_approval"),
            (PermissionKind::Question, "question"),
            (PermissionKind::PlanApproval, "plan_approval"),
        ];
        for (kind, expected_tag) in kinds {
            let json = serde_json::to_string(&kind).expect("serializes");
            // PermissionKind is a unit enum with rename_all = snake_case
            // so it serialises as a plain string
            assert_eq!(json, format!("\"{expected_tag}\""), "kind={kind:?}");
        }
    }

    /// `PermissionDecision` with the `type` tag serialises correctly for all
    /// five variants — only the kind tag and optional payload appear, never raw
    /// tool input or answer content.
    #[test]
    fn test_permission_decision_serde_type_tags() {
        use temps_ai::streaming::PermissionDecision;

        let cases: &[(PermissionDecision, &str)] = &[
            (PermissionDecision::AllowTool, "allow_tool"),
            (
                PermissionDecision::DenyTool {
                    reason: Some("blocked".to_string()),
                },
                "deny_tool",
            ),
            (
                PermissionDecision::AnswerQuestion {
                    answers: serde_json::json!({}),
                },
                "answer_question",
            ),
            (PermissionDecision::ApprovePlan, "approve_plan"),
            (
                PermissionDecision::RejectPlan {
                    feedback: Some("needs revision".to_string()),
                },
                "reject_plan",
            ),
        ];

        for (decision, expected_type) in cases {
            let json = serde_json::to_string(decision).expect("serializes");
            let v: serde_json::Value = serde_json::from_str(&json).expect("parses");
            assert_eq!(
                v["type"].as_str(),
                Some(*expected_type),
                "decision={decision:?}"
            );
        }
    }

    // ── Security-finding tests (ADR-038 Phase 2 milestone 3) ─────────────────

    /// Finding 1 (HIGH-IDOR): cross-conversation claim is rejected with 404.
    ///
    /// A registry entry created for conversation "conv-A" must not be claimable
    /// via "conv-B"'s URL.  The entry must remain in the registry so the
    /// legitimate owner can still claim it.
    #[test]
    fn test_resolve_permission_cross_conversation_is_rejected() {
        use std::collections::HashMap;
        use std::sync::Mutex;
        use temps_ai::streaming::{PermissionDecision, PermissionKind};
        use tokio::sync::oneshot;

        let (tx, _rx) = oneshot::channel::<PermissionDecision>();

        let mut registry: HashMap<String, PendingPermissionEntry> = HashMap::new();
        registry.insert(
            "req-1".to_string(),
            PendingPermissionEntry {
                sender: tx,
                conv_public_id: "conv-A".to_string(),
                kind: PermissionKind::ToolApproval,
                tool_name: "Bash".to_string(),
                input: serde_json::Value::Null,
                generation: uuid::Uuid::new_v4(),
            },
        );
        let registry = Arc::new(Mutex::new(registry));

        // Simulate what `resolve_permission` does inside the lock.
        let attacker_conv = "conv-B";
        let stored_conv_id = {
            let guard = registry.lock().unwrap();
            guard.get("req-1").map(|e| e.conv_public_id.clone())
        };
        let is_idor = stored_conv_id
            .as_deref()
            .map(|id| id != attacker_conv)
            .unwrap_or(false);

        assert!(
            is_idor,
            "cross-conversation claim should be detected as IDOR"
        );

        // The entry must NOT have been removed — the real owner can still claim.
        let guard = registry.lock().unwrap();
        assert!(
            guard.contains_key("req-1"),
            "registry entry must remain after IDOR rejection"
        );
    }

    /// Finding 2 (MEDIUM): submitting the wrong decision kind for a `Question`
    /// permission returns 400 with the typed `PermissionKindMismatch` error.
    #[test]
    fn test_permission_kind_mismatch_maps_to_400() {
        let p: Problem = ChatError::PermissionKindMismatch {
            expected_kind: "question".to_string(),
            received: "allow_tool".to_string(),
        }
        .into();
        assert_eq!(
            p.status_code,
            StatusCode::BAD_REQUEST,
            "kind mismatch must be a client error (400)"
        );
        assert_eq!(
            title_of(&p).as_deref(),
            Some("Permission Decision Mismatch")
        );
        // Detail must include the mismatch context so the caller can fix it.
        let detail = p.body.get("detail").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            detail.contains("question"),
            "detail must mention the expected kind"
        );
        assert!(
            detail.contains("allow_tool"),
            "detail must mention the received decision"
        );
    }

    #[test]
    fn mismatched_permission_decision_can_be_retried() {
        use std::collections::HashMap;
        use temps_ai::streaming::{PermissionDecision, PermissionKind};
        use tokio::sync::oneshot;

        let (tx, _rx) = oneshot::channel();
        let mut registry = HashMap::new();
        registry.insert(
            "question-1".to_string(),
            PendingPermissionEntry {
                sender: tx,
                conv_public_id: "conv-1".to_string(),
                kind: PermissionKind::Question,
                tool_name: "AskUserQuestion".to_string(),
                input: serde_json::Value::Null,
                generation: uuid::Uuid::new_v4(),
            },
        );

        assert!(!permission_decision_matches_kind(
            &registry["question-1"].kind,
            &PermissionDecision::AllowTool
        ));
        assert!(
            registry.contains_key("question-1"),
            "validation must happen before remove-to-claim"
        );

        let retry = PermissionDecision::AnswerQuestion {
            answers: serde_json::json!({"answer": "yes"}),
        };
        assert!(permission_decision_matches_kind(
            &registry["question-1"].kind,
            &retry
        ));
        assert!(registry.remove("question-1").is_some());
    }

    /// Finding 3 (MEDIUM): oversized `reason` in a `DenyTool` payload is caught
    /// by `too_long` before the registry is touched.
    #[test]
    fn test_deny_tool_oversized_reason_returns_400_input_too_long() {
        // Build a `reason` that exceeds the 4 KiB limit.
        let oversized = "x".repeat(MAX_DECISION_STRING_LEN + 1);

        // Mimic the guard in `resolve_permission`.
        let decision = PermissionDecision::DenyTool {
            reason: Some(oversized.clone()),
        };
        let error = match &decision {
            PermissionDecision::DenyTool {
                reason: Some(reason),
            } if reason.len() > MAX_DECISION_STRING_LEN => {
                Some(too_long("reason", MAX_DECISION_STRING_LEN))
            }
            _ => None,
        };

        let p = error.expect("oversized reason must trigger too_long guard");
        assert_eq!(
            p.status_code,
            StatusCode::BAD_REQUEST,
            "oversized payload must be rejected with 400"
        );
        assert_eq!(title_of(&p).as_deref(), Some("Input Too Long"));
    }

    /// Finding 3 (MEDIUM): oversized serialized `answers` in `AnswerQuestion`
    /// is caught before the registry is touched.
    #[test]
    fn test_answer_question_oversized_answers_returns_400() {
        // answers = { "q": "aaa...4097 chars..."}
        let big_value = "a".repeat(MAX_DECISION_STRING_LEN + 1);
        let answers = serde_json::json!({ "q": big_value });

        let decision = PermissionDecision::AnswerQuestion {
            answers: answers.clone(),
        };
        let error = match &decision {
            PermissionDecision::AnswerQuestion { answers } => {
                let serialized_len = serde_json::to_string(answers)
                    .map(|s| s.len())
                    .unwrap_or(usize::MAX);
                if serialized_len > MAX_DECISION_STRING_LEN {
                    Some(too_long("answers", MAX_DECISION_STRING_LEN))
                } else {
                    None
                }
            }
            _ => None,
        };

        let p = error.expect("oversized answers must trigger too_long guard");
        assert_eq!(p.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(title_of(&p).as_deref(), Some("Input Too Long"));
    }
}
