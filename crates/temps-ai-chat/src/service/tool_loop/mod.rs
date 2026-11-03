// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The agentic tool loop behind a chat turn.

use super::*;

mod budget;
mod dispatch;
mod finish;
mod interactions;
mod round;
mod salvage;
mod state;

use budget::*;
use interactions::*;
use state::*;

/// Where a turn's events go: the SSE viewer that started it, if still attached,
/// and every live subscriber to the conversation.
#[derive(Clone)]
struct TurnEvents {
    tx: tokio::sync::mpsc::UnboundedSender<Result<ChatStreamEvent, ChatError>>,
    live: broadcast::Sender<WireEvent>,
}

impl TurnEvents {
    fn emit(&self, item: Result<ChatStreamEvent, ChatError>) {
        emit_turn_event(&self.tx, &self.live, item);
    }
}

/// What a running turn knows about itself. Fixed for the whole turn.
struct TurnContext {
    ai: Arc<dyn AiService>,
    db: Arc<DatabaseConnection>,
    conversation_id: i64,
    project_id: Option<i32>,
    provider_id: String,
    model: String,
    thinking_level: Option<String>,
    /// The mode the provider was launched in. The active mode can change during
    /// the turn; see [`TurnAuthorization::permission_mode`].
    permission_mode: String,
    context_type: String,
    harness_workspace: Option<temps_ai::HarnessWorkspace>,
    sandbox_environment: temps_ai::SensitiveEnvironment,
    sandbox_attachments: Vec<temps_ai::SandboxAttachment>,
    authorization: TurnAuthorization,
    interactions: temps_ai::InteractionExecutor,
    tools: Vec<ChatTool>,
    capture_session_title: bool,
    initial_title: Option<String>,
    draft_message_id: Option<i64>,
    turn_id: Option<String>,
    events: TurnEvents,
}

impl TurnContext {
    /// How logs name this turn; unit-level callers do not claim a durable one.
    fn trace_id(&self) -> &str {
        self.turn_id.as_deref().unwrap_or("untracked")
    }
}

/// One turn in progress: what it is, what it can call, and how far it has got.
struct ToolLoop {
    ctx: TurnContext,
    router: ToolRouter,
    /// Handed to the provider, which runs some tools itself.
    tool_executor: ToolExecutor,
    harness_mcp_server: Option<HarnessMcpServer>,
    /// Keeps the MCP bridge registered for as long as the turn runs.
    _harness_mcp_guard: Option<HarnessMcpGuard>,
    harness_mcp_events: Option<tokio::sync::mpsc::Receiver<ChatStreamDelta>>,
    budget: TurnBudget,
    transcript: TurnTranscript,
    outcome: TurnOutcome,
    session: ProviderSession,
    /// The conversation so far, replayed to the model on each request.
    messages: Vec<ChatMessage>,
}

/// The credential that started a turn no longer authorizes it. Already logged
/// and reported to the client; the turn ends without persisting anything.
struct TurnRevoked;

impl ToolLoop {
    /// Run the turn to its end. Returns whether it failed; a revoked credential
    /// fails it where it stands, with nothing persisted.
    async fn run(mut self) -> bool {
        self.run_stages().await.unwrap_or(true)
    }

    async fn run_stages(&mut self) -> Result<bool, TurnRevoked> {
        tracing::info!(
            component = "ai_turn_timing",
            turn_id = self.ctx.trace_id(),
            conversation_id = self.ctx.conversation_id,
            project_id = self.ctx.project_id,
            provider = %self.ctx.provider_id,
            phase = "execution_loop_started",
            total_ms = 0_u64,
            "AI turn timing"
        );
        self.run_rounds().await?;
        self.conclude_rounds();
        self.salvage_if_unanswered().await?;
        Ok(self.finish().await)
    }

    /// Save the message under construction, if this turn has a draft. Returns
    /// whether it does; a failure to save is logged, and the turn goes on.
    async fn persist_draft(&self, what: &str) -> bool {
        let Some(message_id) = self.ctx.draft_message_id else {
            return false;
        };
        if let Err(error) = persist_assistant_message(
            self.ctx.db.as_ref(),
            message_id,
            &self.transcript.content,
            &self.transcript.tools,
            &self.transcript.parts,
            &self.transcript.open_text,
            true,
        )
        .await
        {
            tracing::warn!(
                conv_id = self.ctx.conversation_id,
                message_id,
                %error,
                "failed to checkpoint {what}"
            );
        }
        true
    }

    async fn checkpoint(&mut self, what: &str) {
        if self.persist_draft(what).await {
            self.transcript.mark_checkpointed();
        }
    }

    /// A harness turn re-checks its credential before each thing it starts.
    async fn check_harness_authorization(&self, stopped: &'static str) -> Result<(), TurnRevoked> {
        if self.ctx.harness_workspace.is_none() {
            return Ok(());
        }
        match self.ctx.authorization.refresh_harness().await {
            Ok(_) => Ok(()),
            Err(error) => Err(self.revoked(error, stopped)),
        }
    }

    fn revoked(&self, error: ToolAuthorizationRefreshError, stopped: &'static str) -> TurnRevoked {
        tracing::warn!(
            conversation_id = self.ctx.conversation_id,
            %error,
            "{stopped}"
        );
        self.ctx
            .events
            .emit(Err(ChatError::AuthorizationRefresh(error)));
        TurnRevoked
    }

    /// Say why the rounds stopped, when it was a bound rather than the model
    /// finishing, before anything else is written.
    ///
    /// Without this the user gets a confident-looking summary with no indication
    /// it was written under duress, which reads as a complete answer and is the
    /// same class of dishonesty as claiming a backtest that never ran. They can
    /// then decide to ask it to continue.
    fn conclude_rounds(&mut self) {
        // Fell out of the loop by exhausting the backstop rather than by
        // answering: worth saying, since it means the task was cut short.
        if self.budget.backstop_exhausted()
            && !self.outcome.answered
            && self.outcome.stop_reason.is_none()
            && !self.transcript.tools.is_empty()
        {
            self.outcome.stop_reason = Some(ROUND_BACKSTOP_REASON);
        }
        self.transcript.close_open_text();
        if let Some(reason) = self.outcome.stop_reason {
            let note = self.transcript.push_stop_note(reason);
            self.ctx.events.emit(Ok(ChatStreamEvent::Token(note)));
        }
    }
}

impl ConversationService {
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
    pub(super) async fn try_tool_loop(
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
            Vec::new(),
            None,
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            None,
            false,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn try_tool_loop_in_workspace(
        &self,
        conv: &ai_conversations::Model,
        base_messages: Vec<ChatMessage>,
        provider: Option<Arc<dyn ConversationContextProvider>>,
        tools: Vec<ChatTool>,
        auth: &AuthContext,
        request_metadata: &RequestMetadata,
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
        harness_project_ids: Vec<i32>,
        harness_workspace: Option<temps_ai::HarnessWorkspace>,
        sandbox_environment: temps_ai::SensitiveEnvironment,
        sandbox_attachments: Vec<temps_ai::SandboxAttachment>,
        active_turn_id: Option<String>,
        should_capture_session_title: bool,
        draft_message_id: Option<i64>,
    ) -> Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>> {
        let max_turn_duration = self.turn_time_limit().await;
        let round_limits = RoundLimits::for_context(&conv.context_type);

        #[allow(
            clippy::disallowed_methods,
            reason = "legacy runtime env config; follow-up: move to a DB-backed setting (P0.7 follow-up). TEMPS_INTERNAL_API_URL, only read when no ConfigService is wired"
        )]
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
        let permission_mode = Arc::new(Mutex::new(ai_permission_mode.clone()));
        let task_permission_mode = permission_mode.clone();
        // An autofix incident chat (ADR-054) runs with nobody watching: its
        // platform writes are refused outright instead of waiting for an
        // approval that would never come. Read once per turn from the chat.
        let autofix_incident = crate::autofix::incident_of_conversation(conv.metadata.as_ref());
        let application_id = conv.application_id;
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
        let goal_support_opt = (conv.context_type
            == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE)
            .then(|| self.goal_support.clone())
            .flatten();
        let sandbox_link_opt = self
            .sandbox_link_for(conv)
            .map(|link| link.with_project_access(project_access_checker.clone()));
        let blueprint_support_opt = (conv.context_type
            == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE)
            .then(|| self.blueprint_support.clone())
            .flatten()
            .map(|mut support| {
                support.project_access = project_access_checker.clone();
                support
            });
        // Offered exactly where `temps_write` is: a credential has no use
        // outside an operation that can be proposed.
        let secret_support_opt = write_handle_opt
            .is_some()
            .then(|| self.secret_support.clone())
            .flatten();

        // The turn runs independently of the SSE reader (a browser refresh
        // only drops `rx`), so it must never wait on a slow viewer: a bounded
        // channel would stall the turn itself. The queue holds at most one
        // turn's events, which the turn's own limits bound.
        #[allow(
            clippy::disallowed_methods,
            reason = "one turn's events; the turn must not block on a slow SSE viewer"
        )]
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();

        let events = TurnEvents {
            tx,
            live: self.broadcast_sender_for(conv_id),
        };
        let monitor_live = events.live.clone();
        let monitor_db = self.db.clone();
        let monitor_title = (conv.context_type != "application").then(|| {
            (
                self.ai.clone(),
                conv.project_id,
                conv.ai_provider.clone(),
                conv.ai_model.clone(),
            )
        });
        let active_turns = self.active_turns.clone();
        let harness_mcp_entries = self.harness_mcp_entries.clone();
        let monitor_turn_id = active_turn_id.clone();
        let task_turn_id = active_turn_id.clone();
        let monitor_started = tokio::time::Instant::now();
        let startup_events = events.clone();
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
            let execution_state = Arc::new(tokio::sync::Mutex::new(ToolExecutionState {
                goals: goal_support_opt.clone(),
                secrets: secret_support_opt.clone(),
                sandbox: sandbox_link_opt.clone(),
                blueprints: blueprint_support_opt.clone(),
                incident: autofix_incident.clone().map(|public_id| IncidentToolGuard {
                    public_id,
                    db: db.clone(),
                }),
                ..Default::default()
            }));
            let authorization = TurnAuthorization {
                db: db.clone(),
                auth: auth.clone(),
                project_access_checker: project_access_checker.clone(),
                application_id,
                harness_project_ids: harness_project_ids.clone(),
                provider: ai_provider.clone(),
                permission_mode: task_permission_mode.clone(),
                uses_harness: harness_workspace.is_some(),
            };
            let registrar = permission_registrar(conv_public_id.clone(), pending_permissions);
            let interactions = provider_interactions(
                registrar.clone(),
                task_auto_approve_provider_tools.clone(),
                authorization.clone(),
            );
            let sandbox_interactions = sandbox_interactions(
                interactions.clone(),
                task_auto_approve_provider_tools.clone(),
                events.clone(),
            );
            let tool_router = ToolRouter {
                project_id,
                conversation_id: conv_id,
                context_id,
                project_access_checker: project_access_checker.clone(),
                provider,
                api_tools,
                repo_tools,
                write_handle: write_handle_opt,
                pending: pending_svc_opt.clone(),
                audit: write_support_audit,
                interactions: platform_interactions(PlatformInteractionContext {
                    registrar,
                    auto_approve: task_auto_approve_provider_tools,
                    events: events.clone(),
                    auth: auth.clone(),
                    metadata: request_metadata,
                    db: db.clone(),
                    autofix_incident,
                }),
                state: execution_state.clone(),
            };
            let turn_tool_executor = turn_tool_executor(tool_router.clone(), authorization.clone());
            let principal_id = auth.user_id();
            let process_executor: Option<ManagedProcessExecutor> =
                harness_workspace.as_ref().map(|workspace| {
                    managed_process_executor(
                        ai.clone(),
                        workspace.clone(),
                        authorization.clone(),
                        sandbox_interactions.clone(),
                        sandbox_environment
                            .redaction_values()
                            .cloned()
                            .collect::<Vec<_>>(),
                    )
                });
            let (harness_mcp_server, _harness_mcp_guard, harness_mcp_events) = match (
                harness_workspace.as_ref(),
                harness_internal_api_url.as_deref(),
            ) {
                (Some(_), Some(internal_api_url)) => {
                    let mut harness_tools = tools.clone();
                    harness_tools.extend(managed_process_tools());
                    let (server, guard, event_rx) = ConversationService::register_harness_mcp(
                        harness_mcp_entries,
                        internal_api_url,
                        principal_id,
                        harness_tools,
                        turn_tool_executor.clone(),
                        process_executor,
                        sandbox_interactions.clone(),
                        max_turn_duration + Duration::from_secs(30),
                    );
                    (Some(server), Some(guard), Some(event_rx))
                }
                _ => (None, None, None),
            };
            let turn = ToolLoop {
                ctx: TurnContext {
                    ai: ai.clone(),
                    db: db.clone(),
                    conversation_id: conv_id,
                    project_id,
                    provider_id: ai_provider.clone(),
                    model: ai_model.clone(),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: ai_permission_mode.clone(),
                    context_type: context_type.clone(),
                    harness_workspace: harness_workspace.clone(),
                    sandbox_environment: sandbox_environment.clone(),
                    sandbox_attachments: sandbox_attachments.clone(),
                    authorization: authorization.clone(),
                    interactions: interactions.clone(),
                    tools: tools.clone(),
                    capture_session_title: should_capture_session_title,
                    initial_title: initial_conversation_title,
                    draft_message_id,
                    turn_id: task_turn_id.clone(),
                    events: events.clone(),
                },
                router: tool_router.clone(),
                tool_executor: turn_tool_executor,
                harness_mcp_server,
                _harness_mcp_guard,
                harness_mcp_events,
                budget: TurnBudget::start(round_limits, max_turn_duration),
                transcript: TurnTranscript::default(),
                outcome: TurnOutcome::default(),
                session: ProviderSession::new(resume_session_id, harness_workspace.is_some()),
                messages: base_messages,
            };
            turn.run().await
        });

        if let Some(turn_id) = active_turn_id.as_ref() {
            let active = ActiveTurn {
                turn_id: turn_id.clone(),
                abort: turn_task.abort_handle(),
                auto_approve_provider_tools: auto_approve_provider_tools.clone(),
                permission_mode: permission_mode.clone(),
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
                    match permission_mode.lock() {
                        Ok(mut mode) => mode.clone_from(&current.ai_permission_mode),
                        Err(poisoned) => poisoned
                            .into_inner()
                            .clone_from(&current.ai_permission_mode),
                    }
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
                    startup_events.emit(Err(ChatError::Ai(
                        "The server could not verify this turn before execution. Please try again."
                            .to_string(),
                    )));
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
                if terminal_status == "failed" {
                    let fallback = crate::PublicChatFailure {
                        code: "turn_failed_without_detail",
                        title: "AI turn failed",
                        detail: "The AI turn stopped before Temps retained a safe failure detail. Retry the message; if it fails again, open session diagnostics.".to_string(),
                        retryable: true,
                    };
                    if let Err(error) = persist_conversation_failure(
                        monitor_db.as_ref(),
                        conv_id,
                        Some(&turn_id),
                        &fallback,
                    )
                    .await
                    {
                        tracing::error!(
                            conversation_id = conv_id,
                            turn_id,
                            %error,
                            "failed to persist fallback AI turn failure"
                        );
                    }
                }
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
                if terminal_status == "completed" {
                    if let Some((ai, project_id, provider, model)) = monitor_title {
                        let db = monitor_db.clone();
                        tokio::spawn(async move {
                            refresh_title_from_conversation(
                                &ai, &db, conv_id, project_id, provider, model,
                            )
                            .await;
                        });
                    }
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
}

#[cfg(test)]
mod tests;
