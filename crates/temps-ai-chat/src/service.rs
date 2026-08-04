//! The conversation service: create/find/history + streaming `send_message`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use chrono::Utc;
use futures::Stream;
use futures_util::StreamExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};

use temps_ai::{AiService, ChatMessage, ChatStreamDelta, ChatTool, ChatTurnRequest, ToolCall};
use temps_auth::context::AuthContext;
use temps_entities::{ai_conversations, ai_messages};

use temps_ai_api_tools::{ApiCallScope, WriteApiToolsHandle, WritePrepareOutcome};

use crate::pending_actions::PendingActionService;
use crate::provider::ConversationContextProvider;
use crate::ChatError;

/// Tool name for the write-proposal (confirm-gated) tool.
const TEMPS_WRITE_TOOL_NAME: &str = "temps_write";

/// System prompt for the one-shot title generator. Kept terse so even small
/// local models return a clean label rather than a sentence.
const TITLE_SYSTEM_PROMPT: &str = "You write a short title for a chat based on the user's first message. \
Reply with ONLY the title: 3–6 words, Title Case, no quotes, no surrounding punctuation, no explanation.";

/// Maximum stored title length (chars). Long titles are truncated, not rejected.
const TITLE_MAX_CHARS: usize = 60;

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
async fn generate_and_store_title(
    ai: &Arc<dyn AiService>,
    db: &Arc<DatabaseConnection>,
    conv_id: i64,
    project_id: i32,
    first_message: &str,
) {
    let req = ChatTurnRequest {
        purpose: "chat.title".to_string(),
        project_id: Some(project_id),
        messages: vec![
            ChatMessage::system(TITLE_SYSTEM_PROMPT),
            ChatMessage::user(format!("First message:\n{first_message}\n\nTitle:")),
        ],
        ..Default::default()
    };
    let raw = match ai.chat(req).await {
        Ok(resp) => resp.content.unwrap_or_default(),
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

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
        }
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

    /// Is AI configured at all? (Capability gate; feature opt-in is checked at the handler.)
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

    /// The active conversation for a context, if one exists.
    pub async fn find_by_context(
        &self,
        project_id: i32,
        context_type: &str,
        context_id: &str,
    ) -> Result<Option<ai_conversations::Model>, ChatError> {
        Ok(ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
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
        conversation_id: i64,
    ) -> Result<ai_conversations::Model, ChatError> {
        ai_conversations::Entity::find_by_id(conversation_id)
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ChatError::NotFound(conversation_id.to_string()))
    }

    /// All active conversations for a project, most-recently-active first. Powers
    /// the conversation switcher.
    pub async fn list_conversations(
        &self,
        project_id: i32,
    ) -> Result<Vec<ai_conversations::Model>, ChatError> {
        Ok(ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
            .filter(ai_conversations::Column::Status.eq("active"))
            .order_by_desc(ai_conversations::Column::LastActivityAt)
            .all(self.db.as_ref())
            .await?)
    }

    /// All active conversations across every project, most-recently-active
    /// first, each annotated with its project's name/slug so the UI can show
    /// where the chat was started and link back to it. Powers the unified
    /// "all chats" switcher.
    ///
    /// Scoping decision: this is **team-visible** — any human with project access
    /// (gated by `ProjectsRead` + non-deployment principal at the handler) sees
    /// that project's chats, matching the instance-wide `ProjectsRead` model and
    /// the dock copy. We deliberately do NOT filter by `created_by`.
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
    pub async fn list_all_conversations(&self) -> Result<Vec<ConversationWithProject>, ChatError> {
        let convs = ai_conversations::Entity::find()
            .filter(ai_conversations::Column::Status.eq("active"))
            .order_by_desc(ai_conversations::Column::LastActivityAt)
            .limit(Self::LIST_ALL_LIMIT)
            .all(self.db.as_ref())
            .await?;

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

    /// A conversation by its public id, scoped to the project.
    pub async fn get_by_public_id(
        &self,
        project_id: i32,
        public_id: &str,
    ) -> Result<ai_conversations::Model, ChatError> {
        ai_conversations::Entity::find()
            .filter(ai_conversations::Column::ProjectId.eq(project_id))
            .filter(ai_conversations::Column::PublicId.eq(public_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ChatError::NotFound(public_id.to_string()))
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

    /// Find or create the conversation for a context (idempotent per active
    /// context). On create, seeds via the provider: a `system` framing message
    /// plus an optional first `assistant` message (e.g. the diagnosis).
    pub async fn get_or_create(
        &self,
        project_id: i32,
        context_type: &str,
        context_id: &str,
        user_id: Option<i32>,
    ) -> Result<ai_conversations::Model, ChatError> {
        if let Some(existing) = self
            .find_by_context(project_id, context_type, context_id)
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
        let conv = ai_conversations::ActiveModel {
            public_id: Set(uuid::Uuid::new_v4().simple().to_string()),
            project_id: Set(project_id),
            context_type: Set(context_type.to_string()),
            context_id: Set(context_id.to_string()),
            title: Set(seed.title.clone()),
            status: Set("active".to_string()),
            created_by: Set(user_id),
            metadata: Set(seed.metadata.clone()),
            created_at: Set(now),
            last_activity_at: Set(now),
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

    async fn touch(&self, conversation_id: i64) {
        let am = ai_conversations::ActiveModel {
            id: Set(conversation_id),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let _ = am.update(self.db.as_ref()).await;
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
        if !self.ai.is_available().await {
            return Err(ChatError::AiUnavailable);
        }
        self.insert_message(conv.id, "user", user_text, None)
            .await?;
        self.touch(conv.id).await;

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
            let first_message = user_text.to_string();
            tokio::spawn(async move {
                generate_and_store_title(&ai, &db, conv_id, project_id, &first_message).await;
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

        // Agentic tool path: gather the tools available for this turn — the
        // context provider's own tools (e.g. a git-backed deployment can read
        // repo files) PLUS the shared, project-scoped trace tools (available in
        // every context when a trace store is configured) PLUS the ADR-024 generic
        // API meta-tools (search_api, describe_api, call_api) registered under the
        // sentinel context_type "__api_tools__". When any tool exists, run a
        // non-streaming tool loop and return the final answer; fall back to plain
        // streaming if the model can't do tools or the loop yields nothing.
        let provider = self.providers.get(conv.context_type.as_str()).cloned();
        let mut tools: Vec<ChatTool> = Vec::new();
        if let Some(p) = &provider {
            tools.extend(p.tools(conv.project_id, &conv.context_id).await);
        }
        // ADR-024: merge the generic API meta-tools from the sentinel provider.
        // This is done for EVERY conversation context so the model can always
        // search/describe/call the read-only REST API, regardless of context_type.
        if let Some(api_tools_provider) = self.providers.get("__api_tools__") {
            tools.extend(
                api_tools_provider
                    .tools(conv.project_id, &conv.context_id)
                    .await,
            );
        }
        // Merge Git-repository exploration tools from the sentinel provider.
        // Gated only by the project having a Git connection (the provider
        // returns an empty vec when not connected). Available in every context
        // (project, alert, deployment, error-group, …) so the model can always
        // explore the source tree when a repo is connected, regardless of which
        // context_type seeded the chat.
        if let Some(repo_tools_provider) = self.providers.get("__repo_tools__") {
            tools.extend(
                repo_tools_provider
                    .tools(conv.project_id, &conv.context_id)
                    .await,
            );
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

        if !tools.is_empty() {
            return Ok(self
                .try_tool_loop(conv, messages, provider, tools, auth)
                .await);
        }

        let req = ChatTurnRequest {
            purpose: format!("chat.{}", conv.context_type),
            project_id: Some(conv.project_id),
            messages,
            ..Default::default()
        };
        let mut token_stream = self
            .ai
            .chat_stream(req)
            .await
            .map_err(|e| ChatError::Ai(e.to_string()))?;

        // Disconnect-safe persistence: drive the AI stream, accumulation, and the
        // final DB insert inside a DETACHED task, relaying tokens to the client
        // over an mpsc channel. If the client (the receiver) disconnects
        // mid-stream the send fails, but the task keeps running to completion and
        // still persists the assistant turn — so a dropped SSE connection never
        // orphans the user turn. The send error is ignored on purpose.
        let db = self.db.clone();
        let conv_id = conv.id;
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();
        tokio::spawn(async move {
            let mut acc = String::new();
            while let Some(item) = token_stream.next().await {
                match item {
                    Ok(tok) => {
                        acc.push_str(&tok);
                        // If the send fails the client disconnected (Stop / navigate
                        // away) — stop pulling tokens so dropping `token_stream`
                        // cancels the upstream provider request, then persist what we
                        // have so the turn isn't orphaned.
                        if tx.send(Ok(ChatStreamEvent::Token(tok))).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(ChatError::Ai(e.to_string())));
                        break;
                    }
                }
            }
            // Persist the assistant turn once the reply is complete (or was stopped),
            // regardless of whether the client is still listening.
            if !acc.is_empty() {
                let am = ai_messages::ActiveModel {
                    conversation_id: Set(conv_id),
                    role: Set("assistant".to_string()),
                    content: Set(acc),
                    created_at: Set(Utc::now()),
                    ..Default::default()
                };
                let _ = am.insert(db.as_ref()).await;
            }
        });
        // Relay channel -> stream. Dropping this stream drops `rx`, which makes
        // `tx.send` fail in the task, but the task continues and persists.
        let out = async_stream::stream! {
            while let Some(item) = rx.recv().await {
                yield item;
            }
        };
        Ok(Box::pin(out))
    }

    /// Load the `ai_write_actions_enabled` flag for a project from the DB.
    /// Best-effort: returns `None` on DB error (caller treats as `false`).
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
                 similarly-named operation. If nothing matches, say so and ask.\n\n\
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
    /// The whole loop runs inside a detached task: if the client drops the SSE
    /// stream the sends start failing (ignored) but the task still runs to
    /// completion and persists the assistant turn, so a dropped connection never
    /// orphans the user turn. We persist `content` (all prose, for history replay)
    /// plus ordered `parts` (text/tool segments in occurrence order) and the
    /// executed `tools`, so a reload renders identically to the live stream.
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

        // Own everything the detached task needs (the service is borrowed `&self`).
        let ai = self.ai.clone();
        let db = self.db.clone();
        let api_tools = self.providers.get("__api_tools__").cloned();
        let repo_tools = self.providers.get("__repo_tools__").cloned();
        let conv_id = conv.id;
        let project_id = conv.project_id;
        let context_type = conv.context_type.clone();
        let context_id = conv.context_id.clone();
        let auth = auth.clone();
        // Write support clones (None when not wired or project toggle is off).
        let write_handle_opt = self
            .write_support
            .as_ref()
            .and_then(|ws| ws.write_handle.get());
        let pending_svc_opt = self.write_support.as_ref().map(|ws| ws.pending.clone());

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();

        // The loop, streaming, and persistence all run in this detached task. When
        // the client drops the SSE stream (Stop, navigate away) `tx.send` fails; we
        // notice that (`client_gone`), stop generating — dropping the AI stream
        // cancels the upstream provider request so a stopped turn stops costing
        // tokens — and persist whatever streamed so far, so the user turn is never
        // orphaned even though it was cut short.
        tokio::spawn(async move {
            let mut messages = base_messages;
            // Anti-repeat guard: maps a (tool name + exact arguments) signature to
            // the result it produced. If the model re-issues an identical call we
            // return a nudge instead of re-running it, so a model that gets stuck
            // can't waste the whole round budget.
            let mut seen_calls: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
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
            // IDs of ai_pending_actions rows created during this turn; linked to the
            // assistant message after it is persisted (best-effort).
            let mut proposed_action_ids: Vec<i64> = Vec::new();
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
                    messages: messages.clone(),
                    tools: tools.clone(),
                    ..Default::default()
                };
                // A single streaming pass: text deltas and tool calls arrive
                // inline. An error here (e.g. the model can't do tools) ends the
                // loop; the salvage below still tries a tool-free reply.
                let mut stream = match ai.chat_stream_turn(req).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("chat_stream_turn failed for conv {conv_id} (round): {e}");
                        last_provider_error = Some(e.to_string());
                        break 'rounds;
                    }
                };
                let mut round_text = String::new();
                let mut round_calls: Vec<ToolCall> = Vec::new();
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
                                    arguments: tc.arguments.clone(),
                                }))
                                .is_err()
                            {
                                client_gone = true;
                                break;
                            }
                            round_calls.push(tc);
                        }
                        Err(e) => {
                            tracing::warn!("chat_stream_turn item error for conv {conv_id}: {e}");
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
                    let call_key = format!("{}|{}", tc.name, tc.arguments.trim());
                    let result = if let Some(prev) = seen_calls.get(&call_key) {
                        format!(
                            "You already ran `{}` with these exact arguments earlier this turn. \
                             Do NOT repeat it. Use a DIFFERENT next step — a different `temps` \
                             command (e.g. another operation, or `<section> --help`) — or answer now \
                             from what you have.\n\nThe previous result was:\n{}",
                            tc.name, prev
                        )
                    } else {
                        let r = if tc.name == TEMPS_WRITE_TOOL_NAME {
                            // Write-proposal path: parse the command, validate (no
                            // execution), stage a pending-action row, return a
                            // JSON proposal receipt to the model.
                            dispatch_write_tool(
                                &tc.arguments,
                                project_id,
                                conv_id,
                                &auth,
                                write_handle_opt.as_deref(),
                                pending_svc_opt.as_deref(),
                                &mut proposed_action_ids,
                                &seen_calls,
                            )
                            .await
                        } else if tc.name == "temps" {
                            if let Some(api_p) = &api_tools {
                                api_p
                                    .execute_tool_with_auth(
                                        project_id,
                                        &context_id,
                                        &tc.name,
                                        &tc.arguments,
                                        &auth,
                                    )
                                    .await
                            } else {
                                format!(
                                    "Tool '{}' is not available (API tools provider absent).",
                                    tc.name
                                )
                            }
                        } else if matches!(
                            tc.name.as_str(),
                            "read_repo_file"
                                | "list_repo_dir"
                                | "list_repo_branches"
                                | "list_repo_tags"
                        ) {
                            // Route Git-repo exploration tools to the sentinel
                            // provider rather than the context provider, so the
                            // model can explore the source tree in any context.
                            if let Some(rt) = &repo_tools {
                                rt.execute_tool(project_id, &context_id, &tc.name, &tc.arguments)
                                    .await
                            } else {
                                format!(
                                    "Tool '{}' is not available (repo tools provider absent).",
                                    tc.name
                                )
                            }
                        } else if let Some(p) = &provider {
                            p.execute_tool(project_id, &context_id, &tc.name, &tc.arguments)
                                .await
                        } else {
                            format!("Tool '{}' is not available in this context.", tc.name)
                        };
                        // Retain only a capped copy. `seen_calls` exists to detect
                        // repeats and to record that a backtest ran — neither
                        // needs the whole payload, and keeping it would hold
                        // every result of the turn at full size in memory while
                        // `trim_carried_tool_results` was busy bounding the very
                        // same data in the transcript. It also means the
                        // repeat-guard cannot resurrect a trimmed result at its
                        // original size.
                        seen_calls.insert(call_key, summarize_for_recall(&r));
                        r
                    };
                    // Surface the result right after — live.
                    if tx
                        .send(Ok(ChatStreamEvent::ToolResult {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: result.clone(),
                        }))
                        .is_err()
                    {
                        client_gone = true;
                    }
                    let tool_part = serde_json::json!({
                        "id": tc.id.clone(),
                        "name": tc.name.clone(),
                        "arguments": tc.arguments.clone(),
                        "result": result.clone(),
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
                    messages: final_messages,
                    ..Default::default()
                };
                let salvage = ai.chat_stream_turn(req).await;
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
            created_at: now,
            last_activity_at: now,
        }
    }

    /// A throwaway admin `AuthContext` for the tool-loop tests. The mock
    /// providers ignore it (they don't override `execute_tool_with_auth`); it
    /// only needs to be a valid value to satisfy the signature.
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
    fn service_with(ai: Arc<ScriptedAi>) -> (ConversationService, Vec<ChatTool>) {
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
                    content: "tool result".to_string(),
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

    // --- service-layer DB tests (MockDatabase) ------------------------------

    /// A `ConversationService` backed by the given mock DB. The AI is a dummy
    /// (`ScriptedAi` with no scripted responses) since these tests exercise only
    /// the DB query/scoping logic, never an AI turn.
    fn db_service(db: DatabaseConnection) -> ConversationService {
        ConversationService {
            db: Arc::new(db),
            ai: Arc::new(ScriptedAi::new(vec![])),
            providers: HashMap::new(),
            write_support: None,
            config: None,
        }
    }

    fn db_service_with_ai(db: DatabaseConnection, ai: Arc<dyn AiService>) -> ConversationService {
        ConversationService {
            db: Arc::new(db),
            ai,
            providers: HashMap::new(),
            write_support: None,
            config: None,
        }
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
            created_at: now,
            last_activity_at: now,
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
            .find_by_context(7, "deployment", "1")
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
            .find_by_context(7, "deployment", "1")
            .await
            .expect("query ok");
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_get_by_id_retains_project_scope() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
            .into_connection();
        let conv = db_service(db).get_by_id(7, 1).await.expect("conversation");
        assert_eq!(conv.public_id, "pubA");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_conversations::Model>::new()])
            .into_connection();
        let err = db_service(db)
            .get_by_id(8, 1)
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

        let convs = svc.list_conversations(7).await.expect("query ok");
        assert_eq!(convs.len(), 2);
        assert!(convs.iter().all(|c| c.project_id == 7));
    }

    // list_all_conversations: annotates each conversation with its project's
    // name/slug, team-visible (no created_by filter).
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

        let items = svc.list_all_conversations().await.expect("query ok");
        assert_eq!(items.len(), 2);
        let alpha = items
            .iter()
            .find(|i| i.conversation.project_id == 7)
            .expect("alpha present");
        assert_eq!(alpha.project_name.as_deref(), Some("Alpha"));
        assert_eq!(alpha.project_slug.as_deref(), Some("alpha"));
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

        let items = svc.list_all_conversations().await.expect("query ok");
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

        let items = svc.list_all_conversations().await.expect("query ok");
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

        let conv = svc.get_by_public_id(7, "pubA").await.expect("found");
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
            .get_by_public_id(99, "pubA")
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
