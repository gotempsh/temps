// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The agentic tool loop behind a chat turn.

use super::*;

mod budget;
mod finish;
mod interactions;
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
    db: Arc<DatabaseConnection>,
    conversation_id: i64,
    project_id: Option<i32>,
    provider_id: String,
    context_type: String,
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
    budget: TurnBudget,
    transcript: TurnTranscript,
    outcome: TurnOutcome,
    session: ProviderSession,
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
        // Directive appended before the final, tool-free answer so the model
        // writes real prose from the evidence instead of narrating another tool
        // call it would like to make.
        const FINAL_DIRECTIVE: &str =
            "You have no more tool calls available. Using ONLY the tool results above, write \
             your final answer to my request now, in plain prose. Do not emit tool-call JSON \
             or describe tools you would call. If the data is insufficient, briefly state what \
             you found and what is still missing.";

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
            let mut messages = base_messages;
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
            let (harness_mcp_server, _harness_mcp_guard, mut harness_mcp_event_rx) = match (
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
            let mut turn = ToolLoop {
                ctx: TurnContext {
                    db: db.clone(),
                    conversation_id: conv_id,
                    project_id,
                    provider_id: ai_provider.clone(),
                    context_type: context_type.clone(),
                    capture_session_title: should_capture_session_title,
                    initial_title: initial_conversation_title,
                    draft_message_id,
                    turn_id: task_turn_id.clone(),
                    events: events.clone(),
                },
                router: tool_router.clone(),
                budget: TurnBudget::start(round_limits, max_turn_duration),
                transcript: TurnTranscript::default(),
                outcome: TurnOutcome::default(),
                session: ProviderSession::new(resume_session_id, harness_workspace.is_some()),
            };
            let ToolLoop {
                budget,
                transcript,
                outcome,
                session,
                ..
            } = &mut turn;
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

            'rounds: for round_index in 0..budget.max_rounds() {
                if budget.deadline_reached() {
                    outcome.stop_reason = Some(TURN_TIMEOUT_REASON);
                    break 'rounds;
                }
                if harness_workspace.is_some() {
                    if let Err(error) = authorization.refresh_harness().await {
                        tracing::warn!(
                            conversation_id = conv_id,
                            %error,
                            "stopped AI turn before provider execution because authorization changed"
                        );
                        events.emit(Err(ChatError::AuthorizationRefresh(error)));
                        return true;
                    }
                }
                let req = ChatTurnRequest {
                    trace_id: task_turn_id.clone(),
                    purpose: format!("chat.{context_type}.tools"),
                    conversation_id: Some(conv_id.to_string()),
                    project_id,
                    principal_id: Some(principal_id),
                    provider: Some(ai_provider.clone()),
                    model: Some(ai_model.clone()),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: Some(ai_permission_mode.clone()),
                    resume_session_id: session.resume_id.clone(),
                    reset_retained_session: session.take_reset(),
                    messages: messages.clone(),
                    sandbox_attachments: sandbox_attachments.clone(),
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
                                        total_ms = budget.elapsed().as_millis() as u64,
                                        "AI turn timing"
                                    );
                        let reason = e.to_string();
                        outcome.observe_error(&e);
                        tracing::warn!(
                            "chat_stream_turn failed for conv {conv_id} (round): {reason}"
                        );
                        if session.may_rebuild(&ai_provider, &reason, false) {
                            tracing::warn!(
                                conversation_id = conv_id,
                                provider = %ai_provider,
                                "provider resume session is missing; rebuilding it from durable conversation history"
                            );
                            session.begin_rebuild();
                            if let Err(error) = clear_missing_provider_session(
                                db.as_ref(),
                                conv_id,
                                task_turn_id.as_deref(),
                            )
                            .await
                            {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    %error,
                                    "failed to clear missing provider session id"
                                );
                            }
                            outcome.retained_diagnostic = None;
                            continue 'rounds;
                        }
                        outcome.remember_error(reason);
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
                        total_ms = budget.elapsed().as_millis() as u64,
                        "AI turn timing"
                    );
                let mut round_text = String::new();
                let mut round_calls: Vec<ToolCall> = Vec::new();
                let mut round_had_effect = false;
                let mut native_tool_ids = std::collections::HashSet::new();
                let mut pending_managed_process_ids = std::collections::HashSet::new();
                let mut provider_stream_finished = false;
                // Did anything this round return usable data (vs. only rejections)?
                let mut round_produced_something = false;
                let mut first_delta_seen = false;
                let mut authorization_check = tokio::time::interval(Duration::from_secs(1));
                authorization_check
                    .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // The turn was checked immediately before provider startup.
                // Consume the interval's immediate first tick so subsequent
                // checks happen once per second while native commands run.
                authorization_check.tick().await;
                loop {
                    if provider_stream_finished && pending_managed_process_ids.is_empty() {
                        break;
                    }
                    let (item, authoritative_mcp_event) = tokio::select! {
                        biased;
                        item = async {
                            match harness_mcp_event_rx.as_mut() {
                                Some(receiver) => receiver.recv().await,
                                None => std::future::pending().await,
                            }
                        } => match item {
                            Some(item) => (Ok(item), true),
                            None => {
                                harness_mcp_event_rx = None;
                                if provider_stream_finished {
                                    break;
                                }
                                continue;
                            },
                        },
                        item = stream.next(), if !provider_stream_finished => match item {
                            Some(item) => (item, false),
                            None => {
                                provider_stream_finished = true;
                                if pending_managed_process_ids.is_empty() {
                                    break;
                                }
                                continue;
                            }
                        },
                        _ = tokio::time::sleep(
                            budget.remaining()
                        ), if provider_stream_finished && !pending_managed_process_ids.is_empty() => {
                            tracing::warn!(
                                conversation_id = conv_id,
                                pending_process_tools = pending_managed_process_ids.len(),
                                "stopped waiting for terminal managed process tool events at the turn deadline"
                            );
                            break;
                        }
                        _ = authorization_check.tick(), if harness_workspace.is_some() => {
                            match authorization.refresh_harness().await {
                                Ok(_) => continue,
                                Err(error) => {
                                    tracing::warn!(
                                        conversation_id = conv_id,
                                        %error,
                                        "stopped active native harness after authorization changed"
                                    );
                                    events.emit(Err(ChatError::AuthorizationRefresh(error)),
                                    );
                                    return true;
                                }
                            }
                        }
                    };
                    if !authoritative_mcp_event
                        && matches!(
                            &item,
                            Ok(ChatStreamDelta::ToolCall(call))
                                if is_managed_process_echo(&call.name)
                        )
                    {
                        continue;
                    }
                    if !authoritative_mcp_event
                        && matches!(
                            &item,
                            Ok(ChatStreamDelta::ToolResult { call, .. })
                                if is_managed_process_echo(&call.name)
                        )
                    {
                        continue;
                    }
                    if !first_delta_seen {
                        first_delta_seen = true;
                        let delta_kind = match &item {
                            Ok(ChatStreamDelta::Text(_)) => "text",
                            Ok(ChatStreamDelta::ContextUsage(_)) => "context_usage",
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
                                        total_ms = budget.elapsed().as_millis() as u64,
                                        "AI turn timing"
                                    );
                    }
                    match item {
                        Ok(ChatStreamDelta::ContextUsage(usage)) => {
                            let usage = PersistedContextWindowUsage::from(usage);
                            if let Err(error) = persist_context_usage(
                                db.as_ref(),
                                conv_id,
                                task_turn_id.as_deref(),
                                &usage,
                            )
                            .await
                            {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    %error,
                                    "failed to persist provider context usage"
                                );
                            }
                            events.emit(Ok(ChatStreamEvent::ContextUsage(usage)));
                        }
                        Ok(ChatStreamDelta::Text(t)) => {
                            round_had_effect |= !t.is_empty();
                            // Separate this round's prose from anything already shown
                            // (e.g. a previous round's narration) with a blank line.
                            if let Some(sep) = transcript.separator_before(&round_text) {
                                transcript.push_open_text(&sep);
                                events.emit(Ok(ChatStreamEvent::Token(sep)));
                            }
                            round_text.push_str(&t);
                            transcript.push_open_text(&t);
                            events.emit(Ok(ChatStreamEvent::Token(t)));
                            if transcript.text_checkpoint_due() {
                                if let Some(message_id) = draft_message_id {
                                    if let Err(error) = persist_assistant_message(
                                        db.as_ref(),
                                        message_id,
                                        &transcript.content,
                                        &transcript.tools,
                                        &transcript.parts,
                                        &transcript.open_text,
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
                                    transcript.mark_checkpointed();
                                }
                            }
                        }
                        Ok(ChatStreamDelta::ToolCall(tc)) => {
                            round_had_effect = true;
                            if authoritative_mcp_event && is_managed_process_tool(&tc.name) {
                                pending_managed_process_ids.insert(tc.id.clone());
                            }
                            let display_name =
                                managed_process_display_name(&tc.name, authoritative_mcp_event);
                            // Surface the call live (the result follows once it runs).
                            let display_arguments = redact_json_string(&tc.arguments);
                            transcript.record_call(&tc.id, &display_name, &display_arguments);
                            if let Some(message_id) = draft_message_id {
                                if let Err(error) = persist_assistant_message(
                                    db.as_ref(),
                                    message_id,
                                    &transcript.content,
                                    &transcript.tools,
                                    &transcript.parts,
                                    &transcript.open_text,
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
                                transcript.mark_checkpointed();
                            }
                            events.emit(Ok(ChatStreamEvent::ToolCall {
                                id: tc.id.clone(),
                                name: display_name,
                                arguments: display_arguments,
                            }));
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
                            round_had_effect = true;
                            if authoritative_mcp_event && is_managed_process_tool(&call.name) {
                                pending_managed_process_ids.remove(&call.id);
                            }
                            let display_name =
                                managed_process_display_name(&call.name, authoritative_mcp_event);
                            native_tool_ids.insert(call.id.clone());
                            let display_arguments = redact_json_string(&call.arguments);
                            let display_result = redact_json_string(&result);
                            events.emit(Ok(ChatStreamEvent::ToolResult {
                                id: call.id.clone(),
                                name: display_name.clone(),
                                content: display_result.clone(),
                            }));
                            transcript.record_result(
                                &call.id,
                                &display_name,
                                &display_arguments,
                                &display_result,
                            );
                            if let Some(message_id) = draft_message_id {
                                if let Err(error) = persist_assistant_message(
                                    db.as_ref(),
                                    message_id,
                                    &transcript.content,
                                    &transcript.tools,
                                    &transcript.parts,
                                    &transcript.open_text,
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
                                transcript.mark_checkpointed();
                            }
                            round_produced_something = true;
                        }
                        Ok(ChatStreamDelta::PermissionRequested(perm)) => {
                            round_had_effect = true;
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
                                    &transcript.content,
                                    &transcript.tools,
                                    &transcript.parts,
                                    &transcript.open_text,
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
                            events.emit(Ok(ChatStreamEvent::PermissionRequested {
                                id: perm.id,
                                kind: perm.kind,
                                tool_name: perm.tool_name,
                                input: perm.input,
                            }));
                        }
                        Ok(ChatStreamDelta::SessionMetadata { session_id, title }) => {
                            session.report(session_id, title);
                        }
                        Err(e) => {
                            let reason = e.to_string();
                            outcome.observe_error(&e);
                            outcome.observe_interruption(&e);
                            tracing::warn!(
                                "chat_stream_turn item error for conv {conv_id}: {reason}"
                            );
                            if session.may_rebuild(&ai_provider, &reason, round_had_effect) {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    provider = %ai_provider,
                                    "provider resume session is missing; rebuilding it from durable conversation history"
                                );
                                session.begin_rebuild();
                                if let Err(error) = clear_missing_provider_session(
                                    db.as_ref(),
                                    conv_id,
                                    task_turn_id.as_deref(),
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conversation_id = conv_id,
                                        %error,
                                        "failed to clear missing provider session id"
                                    );
                                }
                                outcome.retained_diagnostic = None;
                                outcome.stream_interruption = None;
                                continue 'rounds;
                            }
                            // Provider subprocess failures arrive as stream items
                            // after `chat_stream_turn_with_executor` has returned.
                            // Preserve the concrete reason so an empty turn reports
                            // the authentication/model error instead of the generic
                            // "provider returned no response" fallback.
                            outcome.remember_error(reason);
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
                        total_ms = budget.elapsed().as_millis() as u64,
                        failed = outcome.stream_interruption.is_some(),
                        "AI turn timing"
                    );

                if let Some(cause) = outcome.stream_interruption.as_deref() {
                    // Do not execute tool calls from, or ask for another round
                    // after, a reply the provider never finished.
                    tracing::warn!(
                        component = "ai_turn_timing",
                        turn_id = trace_id,
                        conversation_id = conv_id,
                        project_id,
                        provider = %ai_provider,
                        phase = "provider_stream_interrupted",
                        round = round_index + 1,
                        partial_bytes = transcript.content.len() + round_text.len(),
                        failed = true,
                        cause,
                        "AI provider stream ended before the reply was complete"
                    );
                    break 'rounds;
                }

                if !native_tool_ids.is_empty() {
                    round_calls.retain(|call| !native_tool_ids.contains(&call.id));
                    if round_calls.is_empty() {
                        outcome.answered = !round_text.is_empty();
                        break 'rounds;
                    }
                }

                if round_calls.is_empty() {
                    // The model answered in prose — that streamed text is the final
                    // answer. Done.
                    outcome.answered = true;
                    break 'rounds;
                }

                // Record the assistant's tool-call turn for the next round's context.
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: round_text,
                    tool_calls: Some(round_calls.clone()),
                    tool_call_id: None,
                    images: Vec::new(),
                });
                for (call_index, tc) in round_calls.iter().enumerate() {
                    // Route the ADR-024 `temps` CLI tool to the API-tools provider;
                    // `temps_write` to the write-proposal path;
                    // otherwise to the context provider. `project_id` is always the
                    // conversation's project, never anything the model supplied — so
                    // a tool can't be steered to another tenant's data.
                    let result = if call_index >= budget.max_calls_per_round() {
                        // Answer the call rather than dropping it: a provider rejects an
                        // assistant tool_calls block with unanswered ids, and the model
                        // needs to know the call did not run.
                        tool_call_cap_message(budget.max_calls_per_round())
                    } else {
                        match authorization.refresh_tool().await {
                            Ok(fresh_auth) => tool_router.dispatch(tc, &fresh_auth).await,
                            Err(error) => {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    tool = %tc.name,
                                    %error,
                                    "denied conversation tool after authorization refresh failed"
                                );
                                events.emit(Err(ChatError::AuthorizationRefresh(error)));
                                return true;
                            }
                        }
                    };
                    let display_arguments = redact_json_string(&tc.arguments);
                    let display_result = public_tool_result(&tc.name, &result);
                    // Surface the result right after — live.
                    events.emit(Ok(ChatStreamEvent::ToolResult {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        content: display_result.clone(),
                    }));
                    transcript.record_result(&tc.id, &tc.name, &display_arguments, &display_result);
                    if let Some(message_id) = draft_message_id {
                        if let Err(error) = persist_assistant_message(
                            db.as_ref(),
                            message_id,
                            &transcript.content,
                            &transcript.tools,
                            &transcript.parts,
                            &transcript.open_text,
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
                        transcript.mark_checkpointed();
                    }
                    if tool_result_is_productive(&result) {
                        round_produced_something = true;
                    }
                    messages.push(ChatMessage::tool(tc.id.clone(), result));
                }

                if let Some(reason) = budget.record_round(round_produced_something) {
                    outcome.stop_reason = Some(reason);
                    break 'rounds;
                }

                // Bound what gets replayed next round. This, not the round
                // count, is what keeps a long turn from running out of context.
                trim_carried_tool_results(&mut messages, MAX_CARRIED_TOOL_BYTES);
                // Warn the model before the step cap cuts it off, so work still
                // in flight (a deployment building) gets watched instead of
                // promised. It rides on the round's last tool result, which the
                // model reads next and the user never sees.
                if let Some(note) = budget.steps_left_note(round_index) {
                    if let Some(last) = messages.last_mut().filter(|m| m.role == "tool") {
                        last.content.push_str(note);
                    }
                }
                budget.round_completed(round_index);
            }

            // Fell out of the loop by exhausting the backstop rather than by
            // answering — worth saying, since it means the task was cut short.
            if budget.backstop_exhausted()
                && !outcome.answered
                && outcome.stop_reason.is_none()
                && !transcript.tools.is_empty()
            {
                outcome.stop_reason = Some(ROUND_BACKSTOP_REASON);
            }

            // Close any trailing open text part.
            transcript.close_open_text();

            // Say that the turn was cut short, before the salvage answer.
            //
            // Without this the user gets a confident-looking summary with no
            // indication it was written under duress — which reads as a complete
            // answer and is the same class of dishonesty as claiming a backtest
            // that never ran. They can then decide to ask it to continue.
            if let Some(reason) = outcome.stop_reason {
                let note = transcript.push_stop_note(reason);
                events.emit(Ok(ChatStreamEvent::Token(note)));
            }

            // Salvage: the loop used tools but never settled on a prose answer (it
            // hit a bound while still calling tools). Make one tool-free streaming
            // call so the model answers from the evidence it gathered. The result
            // is persisted even when no browser is currently attached.
            if !outcome.answered
                && !transcript.tools.is_empty()
                && outcome.stream_interruption.is_none()
            {
                if harness_workspace.is_some() {
                    if let Err(error) = authorization.refresh_harness().await {
                        tracing::warn!(
                            conversation_id = conv_id,
                            %error,
                            "stopped salvage response because authorization changed"
                        );
                        events.emit(Err(ChatError::AuthorizationRefresh(error)));
                        return true;
                    }
                }
                let mut final_messages = messages;
                final_messages.push(ChatMessage::user(FINAL_DIRECTIVE));
                let req = ChatTurnRequest {
                    trace_id: task_turn_id.clone(),
                    purpose: format!("chat.{context_type}.tools.final"),
                    conversation_id: Some(conv_id.to_string()),
                    project_id,
                    principal_id: Some(principal_id),
                    provider: Some(ai_provider.clone()),
                    model: Some(ai_model.clone()),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: Some(ai_permission_mode.clone()),
                    resume_session_id: session.resume_id.clone(),
                    messages: final_messages,
                    sandbox_attachments,
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
                    outcome.observe_error(e);
                    tracing::warn!("chat_stream_turn failed for conv {conv_id} (salvage): {e}");
                    outcome.remember_error(e.to_string());
                }
                if let Ok(mut stream) = salvage {
                    let mut salvage_text = String::new();
                    let mut authorization_check = tokio::time::interval(Duration::from_secs(1));
                    authorization_check
                        .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    authorization_check.tick().await;
                    loop {
                        let item = tokio::select! {
                            item = stream.next() => match item {
                                Some(item) => item,
                                None => break,
                            },
                            _ = authorization_check.tick(), if harness_workspace.is_some() => {
                                match authorization.refresh_harness().await {
                                    Ok(_) => continue,
                                    Err(error) => {
                                        tracing::warn!(
                                            conversation_id = conv_id,
                                            %error,
                                            "stopped active salvage harness after authorization changed"
                                        );
                                        events.emit(Err(ChatError::AuthorizationRefresh(error)),
                                        );
                                        return true;
                                    }
                                }
                            }
                        };
                        match item {
                            Ok(ChatStreamDelta::Text(t)) => {
                                if let Some(sep) = transcript.separator_before(&salvage_text) {
                                    transcript.content.push_str(&sep);
                                    events.emit(Ok(ChatStreamEvent::Token(sep)));
                                }
                                salvage_text.push_str(&t);
                                transcript.content.push_str(&t);
                                events.emit(Ok(ChatStreamEvent::Token(t)));
                            }
                            Ok(ChatStreamDelta::ContextUsage(usage)) => {
                                let usage = PersistedContextWindowUsage::from(usage);
                                if let Err(error) = persist_context_usage(
                                    db.as_ref(),
                                    conv_id,
                                    task_turn_id.as_deref(),
                                    &usage,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conversation_id = conv_id,
                                        %error,
                                        "failed to persist salvage context usage"
                                    );
                                }
                                events.emit(Ok(ChatStreamEvent::ContextUsage(usage)));
                            }
                            Ok(_) => {}
                            Err(e) => {
                                // The salvage answer is the turn's final text; a
                                // stream cut short here must fail the turn just
                                // like one cut short in a tool round.
                                outcome.observe_interruption(&e);
                                tracing::warn!(
                                    "chat_stream_turn item error for conv {conv_id} (salvage): {e}"
                                );
                                outcome.remember_error(e.to_string());
                                break;
                            }
                        }
                    }
                    if !salvage_text.is_empty() {
                        transcript
                            .parts
                            .push(serde_json::json!({ "type": "text", "text": salvage_text }));
                    }
                }
            }

            turn.finish().await
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
