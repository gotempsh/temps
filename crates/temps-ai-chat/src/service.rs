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

use temps_ai::{AiService, ChatMessage, ChatTool, ChatTurnRequest};
use temps_entities::{ai_conversations, ai_messages};

use crate::provider::ConversationContextProvider;
use crate::trace_tools::TraceTools;
use crate::ChatError;

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

/// Owns conversation persistence + AI turn streaming. Construct once with the
/// registered context providers; resolve via the plugin DI.
pub struct ConversationService {
    db: Arc<DatabaseConnection>,
    ai: Arc<dyn AiService>,
    providers: HashMap<&'static str, Arc<dyn ConversationContextProvider>>,
    /// Shared, project-scoped trace tools (list/inspect distributed traces),
    /// merged into EVERY context's tool loop. `None` when no trace store is
    /// configured (OTel disabled) — then no trace tools are offered.
    trace_tools: Option<TraceTools>,
}

impl ConversationService {
    /// Upper bound on rows returned by the global switcher, so the response (and
    /// the in-memory toggle filter that follows) can't grow without limit.
    const LIST_ALL_LIMIT: u64 = 200;

    pub fn new(
        db: Arc<DatabaseConnection>,
        ai: Arc<dyn AiService>,
        providers: Vec<Arc<dyn ConversationContextProvider>>,
        trace_reader: Option<Arc<dyn temps_core::TraceReader>>,
    ) -> Self {
        let providers = providers
            .into_iter()
            .map(|p| (p.context_type(), p))
            .collect();
        Self {
            db,
            ai,
            providers,
            trace_tools: trace_reader.map(TraceTools::new),
        }
    }

    /// Is AI configured at all? (Capability gate; feature opt-in is checked at the handler.)
    pub async fn ai_available(&self) -> bool {
        self.ai.is_available().await
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
    /// Conversations whose project has `ai_debug_chat_enabled != Some(true)` are
    /// EXCLUDED so a disabled project's chats never surface in the global
    /// switcher — consistent with the per-project read gate, which 403s when the
    /// toggle is off.
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
                let enabled = matches!(p.ai_debug_chat_enabled, Some(true));
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
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>>, ChatError>
    {
        if !self.ai.is_available().await {
            return Err(ChatError::AiUnavailable);
        }
        self.insert_message(conv.id, "user", user_text, None)
            .await?;
        self.touch(conv.id).await;

        let history = self.messages(conv.id).await?;
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

        // Agentic tool path: gather the tools available for this turn — the
        // context provider's own tools (e.g. a git-backed deployment can read
        // repo files) PLUS the shared, project-scoped trace tools (available in
        // every context when a trace store is configured). When any tool exists,
        // run a non-streaming tool loop and return the final answer; fall back to
        // plain streaming if the model can't do tools or the loop yields nothing.
        let provider = self.providers.get(conv.context_type.as_str()).cloned();
        let mut tools: Vec<ChatTool> = Vec::new();
        if let Some(p) = &provider {
            tools.extend(p.tools(conv.project_id, &conv.context_id).await);
        }
        if let Some(tt) = &self.trace_tools {
            tools.extend(tt.tools());
        }
        if !tools.is_empty() {
            if let Some(stream) = self
                .try_tool_loop(conv, &messages, provider.as_ref(), tools)
                .await
            {
                return Ok(stream);
            }
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
                        // Ignore send errors: the client may have disconnected,
                        // but we still want to finish accumulating and persist.
                        let _ = tx.send(Ok(ChatStreamEvent::Token(tok)));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(ChatError::Ai(e.to_string())));
                        break;
                    }
                }
            }
            // Persist the assistant turn once the reply is complete, regardless of
            // whether the client is still listening.
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

    /// One agentic tool loop: call the model with `tools`, execute any tool
    /// calls it makes via the provider, feed results back, and repeat until it
    /// answers in prose (or a round cap is hit). Returns a single-shot stream of
    /// the final answer (and persists it). `None` when the model can't do tools
    /// or never settled on an answer — the caller then falls back to plain
    /// streaming. The conversation *history* replayed to the model is still just
    /// the user→assistant exchange (intermediate tool turns are re-derived each
    /// turn), but the executed tools are persisted on the assistant message's
    /// metadata so the UI can replay them after a reload.
    async fn try_tool_loop(
        &self,
        conv: &ai_conversations::Model,
        base_messages: &[ChatMessage],
        provider: Option<&Arc<dyn ConversationContextProvider>>,
        tools: Vec<ChatTool>,
    ) -> Option<Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>>> {
        const MAX_ROUNDS: usize = 6;
        let mut messages = base_messages.to_vec();
        let mut final_text: Option<String> = None;
        // Buffer the tool activity we observed this turn so it can be replayed to
        // the client BEFORE the final answer. Each entry is a live-only event
        // (`ToolCall` then `ToolResult`); none of it is persisted.
        let mut tool_events: Vec<ChatStreamEvent> = Vec::new();
        // Structured record of each executed tool, persisted on the final
        // assistant message's metadata so the chat replays its tool work after a
        // page reload (not just live during the stream).
        let mut tools_meta: Vec<serde_json::Value> = Vec::new();

        for _ in 0..MAX_ROUNDS {
            let req = ChatTurnRequest {
                purpose: format!("chat.{}.tools", conv.context_type),
                project_id: Some(conv.project_id),
                messages: messages.clone(),
                tools: tools.clone(),
                ..Default::default()
            };
            // An error here (e.g. the model/provider can't do tools) breaks the
            // loop. If we already gathered tool evidence, the salvage call below
            // still lets the model answer from it; if not, the caller falls back
            // to plain streaming.
            let resp = match self.ai.chat(req).await {
                Ok(r) => r,
                Err(_) => break,
            };
            if resp.tool_calls.is_empty() {
                final_text = resp.content;
                break;
            }
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: resp.content.clone().unwrap_or_default(),
                tool_calls: Some(resp.tool_calls.clone()),
                tool_call_id: None,
            });
            for tc in &resp.tool_calls {
                // Surface the invocation just before running it.
                tool_events.push(ChatStreamEvent::ToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: tc.arguments.clone(),
                });
                // Route to the shared trace tools when they own the tool name;
                // otherwise to the context provider. `project_id` is always the
                // conversation's project, never anything the model supplied — so
                // a tool can't be steered to another tenant's data.
                let result =
                    if let Some(tt) = self.trace_tools.as_ref().filter(|tt| tt.handles(&tc.name)) {
                        tt.execute(conv.project_id, &tc.name, &tc.arguments).await
                    } else if let Some(p) = provider {
                        p.execute_tool(conv.project_id, &conv.context_id, &tc.name, &tc.arguments)
                            .await
                    } else {
                        format!("Tool '{}' is not available in this context.", tc.name)
                    };
                // Surface the result right after.
                tool_events.push(ChatStreamEvent::ToolResult {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    content: result.clone(),
                });
                tools_meta.push(serde_json::json!({
                    "id": tc.id.clone(),
                    "name": tc.name.clone(),
                    "arguments": tc.arguments.clone(),
                    "result": result.clone(),
                }));
                messages.push(ChatMessage::tool(tc.id.clone(), result));
            }
        }

        // Salvage: if the loop ended without prose (round cap hit, or a mid-loop
        // provider error) BUT we executed at least one tool, make one final
        // tool-free call so the model answers from the evidence it gathered —
        // instead of the caller discarding it all and answering blind. Bounded:
        // at most one extra call. If it still yields nothing, fall through to the
        // plain-streaming fallback as before.
        if final_text.is_none() && !tools_meta.is_empty() {
            let req = ChatTurnRequest {
                purpose: format!("chat.{}.tools.final", conv.context_type),
                project_id: Some(conv.project_id),
                messages: messages.clone(),
                ..Default::default()
            };
            if let Ok(resp) = self.ai.chat(req).await {
                final_text = resp.content;
            }
        }

        let text = final_text.filter(|t| !t.is_empty())?;
        // Disconnect-safe persistence (same rationale as the plain-streaming
        // path): persist inside a DETACHED task and relay the tool activity plus
        // the single final-answer token over an mpsc channel. The insert runs to
        // completion even if the client dropped the SSE stream, so the final
        // user→assistant exchange is always stored. The executed tools are also
        // persisted on the assistant message metadata so the UI replays them on
        // reload.
        let db = self.db.clone();
        let conv_id = conv.id;
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();
        tokio::spawn(async move {
            for ev in tool_events {
                let _ = tx.send(Ok(ev));
            }
            let _ = tx.send(Ok(ChatStreamEvent::Token(text.clone())));
            let metadata = if tools_meta.is_empty() {
                None
            } else {
                Some(serde_json::json!({ "tools": tools_meta }))
            };
            let am = ai_messages::ActiveModel {
                conversation_id: Set(conv_id),
                role: Set("assistant".to_string()),
                content: Set(text),
                metadata: Set(metadata),
                created_at: Set(Utc::now()),
                ..Default::default()
            };
            let _ = am.insert(db.as_ref()).await;
        });
        let out = async_stream::stream! {
            while let Some(item) = rx.recv().await {
                yield item;
            }
        };
        Some(Box::pin(out))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};

    use temps_ai::{AiError, AiRequest, AiResponse, ChatTurnResponse, TokenStream, ToolCall};

    /// A scripted `AiService`: each `chat()` call pops the next queued response
    /// (or error) so a test can drive the tool loop turn-by-turn, while counting
    /// how many times `chat()` was invoked.
    struct ScriptedAi {
        /// Front-to-back queue of responses for successive `chat()` calls.
        responses: Mutex<std::collections::VecDeque<Result<ChatTurnResponse, AiError>>>,
        chat_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ScriptedAi {
        fn new(responses: Vec<Result<ChatTurnResponse, AiError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                chat_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl AiService for ScriptedAi {
        async fn is_available(&self) -> bool {
            true
        }
        async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
            Err(AiError::NotAvailable)
        }
        async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
            Err(AiError::NotAvailable)
        }
        async fn chat(&self, _request: ChatTurnRequest) -> Result<ChatTurnResponse, AiError> {
            self.chat_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // When the script is exhausted, keep requesting tool calls so a
            // misbehaving loop would run forever — letting MAX_ROUNDS assert.
            self.responses
                .lock()
                .expect("scripted-ai lock")
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(ChatTurnResponse {
                        content: None,
                        tool_calls: vec![ToolCall {
                            id: "loop".to_string(),
                            name: "echo".to_string(),
                            arguments: "{}".to_string(),
                        }],
                    })
                })
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
            trace_tools: None,
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

    // (a) model calls a tool, then returns prose -> tool executed, final text
    // streamed + persisted.
    #[tokio::test]
    async fn test_tool_loop_executes_tool_then_returns_prose() {
        let ai = Arc::new(ScriptedAi::new(vec![
            // Round 1: request a tool call.
            Ok(ChatTurnResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "c1".to_string(),
                    name: "echo".to_string(),
                    arguments: "{}".to_string(),
                }],
            }),
            // Round 2: settle on prose.
            Ok(ChatTurnResponse {
                content: Some("final answer".to_string()),
                tool_calls: vec![],
            }),
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
            .try_tool_loop(&conv, &[], Some(&provider_dyn), tools)
            .await
            .expect("loop should produce a final answer stream");
        let out = drain(stream).await;

        // The scripted tool call surfaces as ToolCall -> ToolResult (live-only),
        // followed by the final assistant prose as a single Token.
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
                ChatStreamEvent::Token("final answer".to_string()),
            ]
        );
        assert_eq!(tool_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(chat_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    // (b) chat() errors on round 1 -> returns None (caller falls back).
    #[tokio::test]
    async fn test_tool_loop_chat_error_returns_none() {
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
        let result = svc
            .try_tool_loop(&conv, &[], Some(&provider_dyn), tools)
            .await;
        assert!(result.is_none());
    }

    // (c) MAX_ROUNDS enforced: a model that always asks for a tool must never
    // exceed MAX_ROUNDS + 1 chat() calls (the rounds plus the single tool-free
    // salvage call), and the loop yields None (no final prose ever materialises).
    #[tokio::test]
    async fn test_tool_loop_enforces_max_rounds() {
        // Empty script: the fallback in ScriptedAi::chat always returns a tool
        // call, so the loop would spin forever without the round cap. The salvage
        // call also gets a tool call (no prose), so the result stays None.
        let ai = Arc::new(ScriptedAi::new(vec![]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let chat_count = ai.chat_calls.clone();
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let result = svc
            .try_tool_loop(&conv, &[], Some(&provider_dyn), tools)
            .await;

        assert!(result.is_none(), "no final prose -> None");
        assert!(
            chat_count.load(std::sync::atomic::Ordering::SeqCst) <= 7,
            "chat() must not exceed MAX_ROUNDS (6) + 1 salvage call"
        );
    }

    // (c2) Salvage: after the loop exhausts MAX_ROUNDS still wanting tools, one
    // final tool-free call lets the model answer from the gathered evidence
    // instead of the caller discarding it. Result is Some(prose), not None.
    #[tokio::test]
    async fn test_tool_loop_salvages_evidence_after_round_cap() {
        // 6 tool-call rounds (never settles on prose), then the 7th (salvage)
        // call finally answers.
        let mut responses: Vec<Result<ChatTurnResponse, AiError>> = (0..6)
            .map(|i| {
                Ok(ChatTurnResponse {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: format!("c{i}"),
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    }],
                })
            })
            .collect();
        responses.push(Ok(ChatTurnResponse {
            content: Some("salvaged answer".to_string()),
            tool_calls: vec![],
        }));
        let ai = Arc::new(ScriptedAi::new(responses));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let chat_count = ai.chat_calls.clone();
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let stream = svc
            .try_tool_loop(&conv, &[], Some(&provider_dyn), tools)
            .await
            .expect("salvage should produce a final answer stream");
        let out = drain(stream).await;

        assert!(
            out.contains(&ChatStreamEvent::Token("salvaged answer".to_string())),
            "final token should be the salvaged answer; got {out:?}"
        );
        assert_eq!(
            chat_count.load(std::sync::atomic::Ordering::SeqCst),
            7,
            "6 tool rounds + 1 salvage call"
        );
    }

    // (d) empty final text -> None.
    #[tokio::test]
    async fn test_tool_loop_empty_final_text_returns_none() {
        let ai = Arc::new(ScriptedAi::new(vec![Ok(ChatTurnResponse {
            content: Some(String::new()),
            tool_calls: vec![],
        })]));
        let provider = Arc::new(StubProvider {
            tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let (svc, tools) = service_with(ai);

        let conv = test_conversation();
        let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
        let result = svc
            .try_tool_loop(&conv, &[], Some(&provider_dyn), tools)
            .await;
        assert!(result.is_none());
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
            trace_tools: None,
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
        }
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

    // list_all_conversations: a conversation whose project has the toggle off (or
    // NULL) is EXCLUDED from the global switcher, even though its row is active.
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
        assert_eq!(items.len(), 1, "only the enabled project's chat survives");
        assert_eq!(items[0].conversation.project_id, 7);
        assert_eq!(items[0].conversation.public_id, "pubEnabled");
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
}
