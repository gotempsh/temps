//! HTTP surface for AI debugging conversations (ADR-023).
//!
//! `GET/POST /projects/{project_id}/ai/conversations` (find / get-or-create),
//! `GET .../{public_id}` (history), `POST .../{public_id}/messages` (SSE stream
//! of the assistant reply), `POST .../{public_id}/archive`. All gated on the
//! per-project `ai_debug_chat_enabled` toggle + AI being configured.

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
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

use temps_auth::{deny_deployment_token, permission_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditLogger, RequestMetadata};
use temps_entities::{ai_conversations, ai_messages};

use crate::audit::{ChatMessageSentAudit, ConversationArchivedAudit, ConversationCreatedAudit};
use crate::service::ChatStreamEvent;
use crate::{ChatError, ConversationService};

/// Shared state for the chat routes.
pub struct AppState {
    pub service: Arc<ConversationService>,
    pub db: Arc<DatabaseConnection>,
    /// Audit logger for write operations (best-effort; never fails a request).
    pub audit_service: Arc<dyn AuditLogger>,
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
        let tools = m
            .metadata
            .as_ref()
            .and_then(|v| v.get("tools"))
            .and_then(|t| serde_json::from_value::<Vec<ToolInfo>>(t.clone()).ok())
            .filter(|t| !t.is_empty());
        let parts = m
            .metadata
            .as_ref()
            .and_then(|v| v.get("parts"))
            .and_then(|p| serde_json::from_value::<Vec<MessagePart>>(p.clone()).ok())
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
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateConversationRequest {
    /// e.g. `"deployment"`.
    pub context_type: String,
    /// The entity id (ints stringified).
    pub context_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FindConversationQuery {
    pub context_type: String,
    pub context_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendMessageRequest {
    pub content: String,
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

// --- error mapping -----------------------------------------------------------

impl From<ChatError> for Problem {
    fn from(e: ChatError) -> Self {
        match e {
            ChatError::NotFound(_) => problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                .with_title("Conversation Not Found")
                .with_detail(e.to_string()),
            ChatError::NoProvider(_) | ChatError::ContextUnavailable => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Context Not Available")
                    .with_detail(e.to_string())
            }
            ChatError::AiUnavailable => problemdetails::new(axum::http::StatusCode::CONFLICT)
                .with_title("AI Not Configured")
                .with_detail(e.to_string()),
            ChatError::Db(_) | ChatError::Ai(_) => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(e.to_string())
            }
        }
    }
}

/// Toggle-only gate: the project must have opted into AI debug chat
/// (`ai_debug_chat_enabled == Some(true)`). Used by the read/archive handlers so
/// that disabling the feature consistently revokes access (returns 403) to
/// existing chat content — reading or archiving history must not require an AI
/// provider to be configured, only the per-project toggle.
async fn ensure_chat_enabled(db: &DatabaseConnection, project_id: i32) -> Result<(), Problem> {
    let project = temps_entities::projects::Entity::find_by_id(project_id)
        .one(db)
        .await
        .map_err(|e| {
            problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_detail(e.to_string())
        })?;
    let enabled = matches!(project.and_then(|p| p.ai_debug_chat_enabled), Some(true));
    if !enabled {
        return Err(problemdetails::new(axum::http::StatusCode::FORBIDDEN)
            .with_title("AI Debug Chat Disabled")
            .with_detail("Enable AI debug chat for this project to use it."));
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
            .with_detail("Configure an AI provider to use debugging chat."));
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

/// 400 for an over-length input field.
fn too_long(field: &str, max: usize) -> Problem {
    problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
        .with_title("Input Too Long")
        .with_detail(format!(
            "'{field}' exceeds the maximum length of {max} characters."
        ))
}

// --- handlers ----------------------------------------------------------------

/// Find the existing chat for a context (returns `null` if none yet). Requires
/// the per-project `ai_debug_chat_enabled` toggle to be on; returns 403 when the
/// feature is disabled so revoking it consistently hides existing chat content.
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
    // Bound the lookup keys, consistent with create_conversation, so oversized
    // query strings can't reach the DB.
    if q.context_type.len() > MAX_CONTEXT_TYPE_LEN {
        return Err(too_long("context_type", MAX_CONTEXT_TYPE_LEN));
    }
    if q.context_id.len() > MAX_CONTEXT_ID_LEN {
        return Err(too_long("context_id", MAX_CONTEXT_ID_LEN));
    }
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let found = state
        .service
        .find_by_context(project_id, &q.context_type, &q.context_id)
        .await?;
    Ok(Json(found.map(ConversationResponse::from)))
}

/// List every active conversation across all projects, most-recently-active
/// first, annotated with project name/slug. Powers the unified "all chats"
/// switcher in the AI assistant dock.
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
    let items = state.service.list_all_conversations().await?;
    Ok(Json(
        items
            .into_iter()
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
            })
            .collect(),
    ))
}

/// List all active conversations for a project, most-recently-active first.
/// Powers the conversation switcher in the AI assistant sidebar.
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
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let conversations = state.service.list_conversations(project_id).await?;
    Ok(Json(
        conversations
            .into_iter()
            .map(ConversationResponse::from)
            .collect(),
    ))
}

/// Get-or-create the chat for a context (seeds it on first open).
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
    if req.context_type.len() > MAX_CONTEXT_TYPE_LEN {
        return Err(too_long("context_type", MAX_CONTEXT_TYPE_LEN));
    }
    if req.context_id.len() > MAX_CONTEXT_ID_LEN {
        return Err(too_long("context_id", MAX_CONTEXT_ID_LEN));
    }
    ensure_enabled(&state, project_id).await?;
    let conv = state
        .service
        .get_or_create(
            project_id,
            &req.context_type,
            &req.context_id,
            Some(auth.user_id()),
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
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let conv = state
        .service
        .get_by_public_id(project_id, &public_id)
        .await?;
    let messages = state
        .service
        .messages(conv.id)
        .await?
        .into_iter()
        .filter(|m| m.role != "system")
        .map(MessageResponse::from)
        .collect();
    Ok(Json(ConversationDetailResponse {
        conversation: ConversationResponse::from(conv),
        messages,
    }))
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
    if req.content.trim().is_empty() {
        return Err(problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("Empty Message")
            .with_detail("Message content must not be empty."));
    }
    if req.content.len() > MAX_MESSAGE_CONTENT_LEN {
        return Err(too_long("content", MAX_MESSAGE_CONTENT_LEN));
    }
    ensure_enabled(&state, project_id).await?;
    let conv = state
        .service
        .get_by_public_id(project_id, &public_id)
        .await?;
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

    let sse = token_stream.map(|item| {
        let event = match item {
            Ok(ChatStreamEvent::Token(text)) => Event::default().data(text),
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
                    Ok(json) => Event::default().event("tool_call").data(json),
                    Err(e) => Event::default()
                        .event("error")
                        .data(format!("failed to encode tool_call event: {e}")),
                }
            }
            Ok(ChatStreamEvent::ToolResult { id, name, content }) => {
                let payload = ToolResultEvent { id, name, content };
                match serde_json::to_string(&payload) {
                    Ok(json) => Event::default().event("tool_result").data(json),
                    Err(e) => Event::default()
                        .event("error")
                        .data(format!("failed to encode tool_result event: {e}")),
                }
            }
            Err(e) => Event::default().event("error").data(e.to_string()),
        };
        Ok::<_, Infallible>(event)
    });
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
    ensure_chat_enabled(state.db.as_ref(), project_id).await?;
    let conv = state
        .service
        .get_by_public_id(project_id, &public_id)
        .await?;
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

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
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
            get(get_conversation),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/messages",
            post(send_message),
        )
        .route(
            "/projects/{project_id}/ai/conversations/{public_id}/archive",
            post(archive_conversation),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        find_conversation,
        list_conversations,
        list_all_conversations,
        create_conversation,
        get_conversation,
        send_message,
        archive_conversation,
    ),
    components(schemas(
        ConversationResponse,
        GlobalConversationResponse,
        MessageResponse,
        ToolInfo,
        MessagePart,
        ConversationDetailResponse,
        CreateConversationRequest,
        SendMessageRequest,
        ToolCallEvent,
        ToolResultEvent,
    ))
)]
pub struct AiChatApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
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
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_debug_chat_enabled: toggle,
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
    async fn test_ensure_chat_enabled_403_when_toggle_off() {
        let db = db_returning(Some(project_with_toggle(7, Some(false))));
        let err = ensure_chat_enabled(&db, 7)
            .await
            .expect_err("toggle off must be denied");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_ensure_chat_enabled_403_when_toggle_null() {
        let db = db_returning(Some(project_with_toggle(7, None)));
        let err = ensure_chat_enabled(&db, 7)
            .await
            .expect_err("toggle null (default off) must be denied");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
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
}
