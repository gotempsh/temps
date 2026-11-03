// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One round of a turn: a single streaming pass that yields assistant text and
//! tool calls inline, then whatever that pass decides comes next.

use super::*;

/// What the turn does after a round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoundOutcome {
    Continue,
    Finished,
}

/// What a provider stream did, once it stopped being read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum StreamEnd {
    /// It ended, or failed in a way the round can still act on.
    Drained,
    /// The session it resumed is gone. Rebuilt; try again.
    SessionRebuilt,
}

enum DeltaEffect {
    Continue,
    StreamFailed,
    SessionRebuilt,
}

/// What one round has seen so far.
pub(super) struct RoundState {
    pub(super) index: usize,
    provider_call_started: tokio::time::Instant,
    pub(super) text: String,
    pub(super) calls: Vec<ToolCall>,
    pub(super) had_effect: bool,
    pub(super) native_tool_ids: std::collections::HashSet<String>,
    pub(super) pending_process_ids: std::collections::HashSet<String>,
    stream_finished: bool,
    /// Did anything this round return usable data (vs. only rejections)?
    pub(super) produced_something: bool,
    first_delta_seen: bool,
}

impl RoundState {
    pub(super) fn new(index: usize) -> Self {
        Self {
            index,
            provider_call_started: tokio::time::Instant::now(),
            text: String::new(),
            calls: Vec::new(),
            had_effect: false,
            native_tool_ids: std::collections::HashSet::new(),
            pending_process_ids: std::collections::HashSet::new(),
            stream_finished: false,
            produced_something: false,
            first_delta_seen: false,
        }
    }
}

fn delta_kind(item: &Result<ChatStreamDelta, temps_ai::AiError>) -> &'static str {
    match item {
        Ok(ChatStreamDelta::Text(_)) => "text",
        Ok(ChatStreamDelta::ContextUsage(_)) => "context_usage",
        Ok(ChatStreamDelta::ToolCall(_)) => "tool_call",
        Ok(ChatStreamDelta::ToolResult { .. }) => "tool_result",
        Ok(ChatStreamDelta::PermissionRequested(_)) => "permission",
        Ok(ChatStreamDelta::SessionMetadata { .. }) => "session_metadata",
        Err(_) => "error",
    }
}

/// A harness may narrate the managed-process tools it reaches over MCP. The
/// MCP bridge's own events are authoritative, so these echoes are dropped.
fn is_provider_echo(item: &Result<ChatStreamDelta, temps_ai::AiError>) -> bool {
    match item {
        Ok(ChatStreamDelta::ToolCall(call)) => is_managed_process_echo(&call.name),
        Ok(ChatStreamDelta::ToolResult { call, .. }) => is_managed_process_echo(&call.name),
        _ => false,
    }
}

impl ToolLoop {
    /// Run rounds until one ends the turn or a bound does. The time limit and
    /// the round ceiling are checked here; everything else that ends a turn is
    /// decided by the round that saw it.
    pub(super) async fn run_rounds(&mut self) -> Result<(), TurnRevoked> {
        for round_index in 0..self.budget.max_rounds() {
            if self.budget.deadline_reached() {
                self.outcome.stop_reason = Some(TURN_TIMEOUT_REASON);
                break;
            }
            self.check_harness_authorization(
                "stopped AI turn before provider execution because authorization changed",
            )
            .await?;
            if self.run_round(round_index).await? == RoundOutcome::Finished {
                break;
            }
        }
        Ok(())
    }

    /// One provider call and what follows from it. The stream stays open until
    /// the round's tool calls have run.
    async fn run_round(&mut self, round_index: usize) -> Result<RoundOutcome, TurnRevoked> {
        let request = self.round_request();
        let mut round = RoundState::new(round_index);
        let mut stream = match self.open_round_stream(request, &round).await {
            Ok(stream) => stream,
            Err(outcome) => return Ok(outcome),
        };
        self.log_round_timing("provider_stream_ready", &round);
        if self.consume_stream(&mut round, &mut stream).await? == StreamEnd::SessionRebuilt {
            return Ok(RoundOutcome::Continue);
        }
        tracing::info!(
            component = "ai_turn_timing",
            turn_id = self.ctx.trace_id(),
            conversation_id = self.ctx.conversation_id,
            project_id = self.ctx.project_id,
            provider = %self.ctx.provider_id,
            phase = "provider_stream_complete",
            round = round.index + 1,
            phase_ms = round.provider_call_started.elapsed().as_millis() as u64,
            total_ms = self.budget.elapsed().as_millis() as u64,
            failed = self.outcome.stream_interruption.is_some(),
            "AI turn timing"
        );
        if self.settle_stream(&mut round) == RoundOutcome::Finished {
            return Ok(RoundOutcome::Finished);
        }
        // Record the assistant's tool-call turn for the next round's context.
        self.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: std::mem::take(&mut round.text),
            tool_calls: Some(round.calls.clone()),
            tool_call_id: None,
            images: Vec::new(),
        });
        let produced = self.dispatch_round_calls(&round.calls).await?;
        Ok(self.spend_round(round.index, round.produced_something || produced))
    }

    fn round_request(&mut self) -> ChatTurnRequest {
        ChatTurnRequest {
            trace_id: self.ctx.turn_id.clone(),
            purpose: format!("chat.{}.tools", self.ctx.context_type),
            conversation_id: Some(self.ctx.conversation_id.to_string()),
            project_id: self.ctx.project_id,
            principal_id: Some(self.ctx.authorization.auth.user_id()),
            provider: Some(self.ctx.provider_id.clone()),
            model: Some(self.ctx.model.clone()),
            thinking_level: self.ctx.thinking_level.clone(),
            permission_mode: Some(self.ctx.permission_mode.clone()),
            resume_session_id: self.session.resume_id.clone(),
            reset_retained_session: self.session.take_reset(),
            messages: self.messages.clone(),
            sandbox_attachments: self.ctx.sandbox_attachments.clone(),
            tools: self.ctx.tools.clone(),
            harness_workspace: self.ctx.harness_workspace.clone(),
            sandbox_environment: self.ctx.sandbox_environment.clone(),
            harness_mcp_server: self.harness_mcp_server.clone(),
            capture_session_title: self.ctx.capture_session_title,
            ..Default::default()
        }
    }

    /// An error here (e.g. the model can't do tools) ends the rounds; the
    /// salvage still tries a tool-free reply.
    async fn open_round_stream(
        &mut self,
        request: ChatTurnRequest,
        round: &RoundState,
    ) -> Result<temps_ai::ChatTurnStream, RoundOutcome> {
        let opened = self
            .ctx
            .ai
            .chat_stream_turn_with_services(
                request,
                temps_ai::TurnServices {
                    tools: Some(self.tool_executor.clone()),
                    interactions: Some(self.ctx.interactions.clone()),
                },
            )
            .await;
        let e = match opened {
            Ok(stream) => return Ok(stream),
            Err(e) => e,
        };
        self.log_round_timing("provider_stream_failed", round);
        let reason = e.to_string();
        self.outcome.observe_error(&e);
        tracing::warn!(
            "chat_stream_turn failed for conv {} (round): {reason}",
            self.ctx.conversation_id
        );
        if self
            .session
            .may_rebuild(&self.ctx.provider_id, &reason, false)
        {
            self.rebuild_missing_session().await;
            return Err(RoundOutcome::Continue);
        }
        self.outcome.remember_error(reason);
        Err(RoundOutcome::Finished)
    }

    /// Read the provider's stream, and the MCP bridge's events alongside it,
    /// until the round has nothing left to wait for.
    pub(super) async fn consume_stream(
        &mut self,
        round: &mut RoundState,
        stream: &mut temps_ai::ChatTurnStream,
    ) -> Result<StreamEnd, TurnRevoked> {
        let mut authorization_check = tokio::time::interval(Duration::from_secs(1));
        authorization_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The turn was checked immediately before provider startup. Consume the
        // interval's immediate first tick so subsequent checks happen once per
        // second while native commands run.
        authorization_check.tick().await;
        loop {
            if round.stream_finished && round.pending_process_ids.is_empty() {
                break;
            }
            let (item, authoritative_mcp_event) = tokio::select! {
                biased;
                item = async {
                    match self.harness_mcp_events.as_mut() {
                        Some(receiver) => receiver.recv().await,
                        None => std::future::pending().await,
                    }
                } => match item {
                    Some(item) => (Ok(item), true),
                    None => {
                        self.harness_mcp_events = None;
                        if round.stream_finished {
                            break;
                        }
                        continue;
                    },
                },
                item = stream.next(), if !round.stream_finished => match item {
                    Some(item) => (item, false),
                    None => {
                        round.stream_finished = true;
                        if round.pending_process_ids.is_empty() {
                            break;
                        }
                        continue;
                    }
                },
                _ = tokio::time::sleep(self.budget.remaining()),
                    if round.stream_finished && !round.pending_process_ids.is_empty() => {
                    tracing::warn!(
                        conversation_id = self.ctx.conversation_id,
                        pending_process_tools = round.pending_process_ids.len(),
                        "stopped waiting for terminal managed process tool events at the turn deadline"
                    );
                    break;
                }
                _ = authorization_check.tick(), if self.ctx.harness_workspace.is_some() => {
                    match self.ctx.authorization.refresh_harness().await {
                        Ok(_) => continue,
                        Err(error) => {
                            return Err(self.revoked(
                                error,
                                "stopped active native harness after authorization changed",
                            ));
                        }
                    }
                }
            };
            if !authoritative_mcp_event && is_provider_echo(&item) {
                continue;
            }
            if !round.first_delta_seen {
                round.first_delta_seen = true;
                tracing::info!(
                    component = "ai_turn_timing",
                    turn_id = self.ctx.trace_id(),
                    conversation_id = self.ctx.conversation_id,
                    project_id = self.ctx.project_id,
                    provider = %self.ctx.provider_id,
                    phase = "provider_first_delta",
                    round = round.index + 1,
                    delta_kind = delta_kind(&item),
                    phase_ms = round.provider_call_started.elapsed().as_millis() as u64,
                    total_ms = self.budget.elapsed().as_millis() as u64,
                    "AI turn timing"
                );
            }
            match self.apply_delta(round, item, authoritative_mcp_event).await {
                DeltaEffect::Continue => {}
                DeltaEffect::StreamFailed => break,
                DeltaEffect::SessionRebuilt => return Ok(StreamEnd::SessionRebuilt),
            }
        }
        Ok(StreamEnd::Drained)
    }

    async fn apply_delta(
        &mut self,
        round: &mut RoundState,
        item: Result<ChatStreamDelta, temps_ai::AiError>,
        authoritative_mcp_event: bool,
    ) -> DeltaEffect {
        match item {
            Ok(ChatStreamDelta::ContextUsage(usage)) => self.on_context_usage(usage).await,
            Ok(ChatStreamDelta::Text(text)) => self.on_text(round, text).await,
            Ok(ChatStreamDelta::ToolCall(call)) => {
                self.on_tool_call(round, call, authoritative_mcp_event)
                    .await;
            }
            Ok(ChatStreamDelta::ToolResult { call, result }) => {
                self.on_tool_result(round, call, result, authoritative_mcp_event)
                    .await;
            }
            Ok(ChatStreamDelta::PermissionRequested(permission)) => {
                self.on_permission_requested(round, permission).await;
            }
            Ok(ChatStreamDelta::SessionMetadata { session_id, title }) => {
                self.session.report(session_id, title);
            }
            Err(error) => return self.on_stream_error(round, error).await,
        }
        DeltaEffect::Continue
    }

    async fn on_context_usage(&mut self, usage: temps_ai::ContextWindowUsage) {
        let usage = PersistedContextWindowUsage::from(usage);
        if let Err(error) = persist_context_usage(
            self.ctx.db.as_ref(),
            self.ctx.conversation_id,
            self.ctx.turn_id.as_deref(),
            &usage,
        )
        .await
        {
            tracing::warn!(
                conversation_id = self.ctx.conversation_id,
                %error,
                "failed to persist provider context usage"
            );
        }
        self.ctx
            .events
            .emit(Ok(ChatStreamEvent::ContextUsage(usage)));
    }

    async fn on_text(&mut self, round: &mut RoundState, text: String) {
        round.had_effect |= !text.is_empty();
        if let Some(sep) = self.transcript.separator_before(&round.text) {
            self.transcript.push_open_text(&sep);
            self.ctx.events.emit(Ok(ChatStreamEvent::Token(sep)));
        }
        round.text.push_str(&text);
        self.transcript.push_open_text(&text);
        self.ctx.events.emit(Ok(ChatStreamEvent::Token(text)));
        if self.transcript.text_checkpoint_due() {
            self.checkpoint("assistant text").await;
        }
    }

    async fn on_tool_call(
        &mut self,
        round: &mut RoundState,
        call: ToolCall,
        authoritative_mcp_event: bool,
    ) {
        round.had_effect = true;
        if authoritative_mcp_event && is_managed_process_tool(&call.name) {
            round.pending_process_ids.insert(call.id.clone());
        }
        let display_name = managed_process_display_name(&call.name, authoritative_mcp_event);
        // Surface the call live (the result follows once it runs).
        let display_arguments = redact_json_string(&call.arguments);
        self.transcript
            .record_call(&call.id, &display_name, &display_arguments);
        self.checkpoint("assistant tool call").await;
        self.ctx.events.emit(Ok(ChatStreamEvent::ToolCall {
            id: call.id.clone(),
            name: display_name,
            arguments: display_arguments,
        }));
        // A sandbox harness has already executed this native tool itself. In
        // particular, a background Bash job may not emit a terminal tool_result
        // until the process exits. Never hand that call to the outer API-tool
        // executor while waiting: doing so replays unknown native tools for up to
        // the 500-round anti-spin guard and produces the misleading "unusually
        // long run" fallback.
        if self.ctx.harness_workspace.is_some() {
            round.native_tool_ids.insert(call.id.clone());
        }
        round.calls.push(call);
    }

    async fn on_tool_result(
        &mut self,
        round: &mut RoundState,
        call: ToolCall,
        result: String,
        authoritative_mcp_event: bool,
    ) {
        round.had_effect = true;
        if authoritative_mcp_event && is_managed_process_tool(&call.name) {
            round.pending_process_ids.remove(&call.id);
        }
        let display_name = managed_process_display_name(&call.name, authoritative_mcp_event);
        round.native_tool_ids.insert(call.id.clone());
        let display_arguments = redact_json_string(&call.arguments);
        let display_result = redact_json_string(&result);
        self.ctx.events.emit(Ok(ChatStreamEvent::ToolResult {
            id: call.id.clone(),
            name: display_name.clone(),
            content: display_result.clone(),
        }));
        self.transcript
            .record_result(&call.id, &display_name, &display_arguments, &display_result);
        self.checkpoint("assistant tool result").await;
        round.produced_something = true;
    }

    /// The gateway AI path (OpenAI/Anthropic API) never emits this: it only comes
    /// from `run_interactive` on the interactive CLI path, which has its own
    /// streaming channel. If it somehow arrives here, forward it to the client
    /// (harmless) and log so operators can investigate.
    async fn on_permission_requested(
        &mut self,
        round: &mut RoundState,
        permission: PermissionRequest,
    ) {
        round.had_effect = true;
        tracing::warn!(
            conv_id = self.ctx.conversation_id,
            permission_id = %permission.id,
            tool_name = %permission.tool_name,
            "PermissionRequested delta arrived in the gateway tool loop \
             (unexpected — only expected on the interactive CLI path); \
             forwarding to client"
        );
        self.persist_draft("assistant before permission request")
            .await;
        self.ctx
            .events
            .emit(Ok(ChatStreamEvent::PermissionRequested {
                id: permission.id,
                kind: permission.kind,
                tool_name: permission.tool_name,
                input: permission.input,
            }));
    }

    async fn on_stream_error(
        &mut self,
        round: &RoundState,
        error: temps_ai::AiError,
    ) -> DeltaEffect {
        let reason = error.to_string();
        self.outcome.observe_error(&error);
        self.outcome.observe_interruption(&error);
        tracing::warn!(
            "chat_stream_turn item error for conv {}: {reason}",
            self.ctx.conversation_id
        );
        if self
            .session
            .may_rebuild(&self.ctx.provider_id, &reason, round.had_effect)
        {
            self.rebuild_missing_session().await;
            return DeltaEffect::SessionRebuilt;
        }
        // Provider subprocess failures arrive as stream items after
        // `chat_stream_turn_with_executor` has returned. Preserve the concrete
        // reason so an empty turn reports the authentication/model error instead
        // of the generic "provider returned no response" fallback.
        self.outcome.remember_error(reason);
        DeltaEffect::StreamFailed
    }

    async fn rebuild_missing_session(&mut self) {
        tracing::warn!(
            conversation_id = self.ctx.conversation_id,
            provider = %self.ctx.provider_id,
            "provider resume session is missing; rebuilding it from durable conversation history"
        );
        self.session.begin_rebuild();
        if let Err(error) = clear_missing_provider_session(
            self.ctx.db.as_ref(),
            self.ctx.conversation_id,
            self.ctx.turn_id.as_deref(),
        )
        .await
        {
            tracing::warn!(
                conversation_id = self.ctx.conversation_id,
                %error,
                "failed to clear missing provider session id"
            );
        }
        self.outcome.retained_diagnostic = None;
        self.outcome.stream_interruption = None;
    }

    /// Decide what the stream leaves the round to do. Only calls the provider
    /// left to us go on to dispatch.
    pub(super) fn settle_stream(&mut self, round: &mut RoundState) -> RoundOutcome {
        if let Some(cause) = self.outcome.stream_interruption.as_deref() {
            // Do not execute tool calls from, or ask for another round after, a
            // reply the provider never finished.
            tracing::warn!(
                component = "ai_turn_timing",
                turn_id = self.ctx.trace_id(),
                conversation_id = self.ctx.conversation_id,
                project_id = self.ctx.project_id,
                provider = %self.ctx.provider_id,
                phase = "provider_stream_interrupted",
                round = round.index + 1,
                partial_bytes = self.transcript.content.len() + round.text.len(),
                failed = true,
                cause,
                "AI provider stream ended before the reply was complete"
            );
            return RoundOutcome::Finished;
        }
        if !round.native_tool_ids.is_empty() {
            round
                .calls
                .retain(|call| !round.native_tool_ids.contains(&call.id));
            if round.calls.is_empty() {
                self.outcome.answered = !round.text.is_empty();
                return RoundOutcome::Finished;
            }
        }
        if round.calls.is_empty() {
            // The model answered in prose: that streamed text is the final
            // answer. Done.
            self.outcome.answered = true;
            return RoundOutcome::Finished;
        }
        RoundOutcome::Continue
    }

    /// Charge a dispatched round to the budget and prepare the next one.
    pub(super) fn spend_round(&mut self, round_index: usize, produced: bool) -> RoundOutcome {
        if let Some(reason) = self.budget.record_round(produced) {
            self.outcome.stop_reason = Some(reason);
            return RoundOutcome::Finished;
        }
        // Bound what gets replayed next round. This, not the round count, is
        // what keeps a long turn from running out of context.
        trim_carried_tool_results(&mut self.messages, MAX_CARRIED_TOOL_BYTES);
        // Warn the model before the step cap cuts it off, so work still in flight
        // (a deployment building) gets watched instead of promised. It rides on
        // the round's last tool result, which the model reads next and the user
        // never sees.
        if let Some(note) = self.budget.steps_left_note(round_index) {
            if let Some(last) = self.messages.last_mut().filter(|m| m.role == "tool") {
                last.content.push_str(note);
            }
        }
        self.budget.round_completed(round_index);
        RoundOutcome::Continue
    }

    fn log_round_timing(&self, phase: &'static str, round: &RoundState) {
        tracing::info!(
            component = "ai_turn_timing",
            turn_id = self.ctx.trace_id(),
            conversation_id = self.ctx.conversation_id,
            project_id = self.ctx.project_id,
            provider = %self.ctx.provider_id,
            phase,
            round = round.index + 1,
            phase_ms = round.provider_call_started.elapsed().as_millis() as u64,
            total_ms = self.budget.elapsed().as_millis() as u64,
            "AI turn timing"
        );
    }
}
