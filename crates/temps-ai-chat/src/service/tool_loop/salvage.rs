// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Salvage: the loop used tools but never settled on a prose answer (it hit a
//! bound while still calling tools). One tool-free streaming call lets the model
//! answer from the evidence it gathered. The result is persisted even when no
//! browser is currently attached.

use super::*;

/// Appended before the final, tool-free answer so the model writes real prose
/// from the evidence instead of narrating another tool call it would like to
/// make.
pub(super) const FINAL_DIRECTIVE: &str =
    "You have no more tool calls available. Using ONLY the tool results above, write \
     your final answer to my request now, in plain prose. Do not emit tool-call JSON \
     or describe tools you would call. If the data is insufficient, briefly state what \
     you found and what is still missing.";

impl ToolLoop {
    pub(super) async fn salvage_if_unanswered(&mut self) -> Result<(), TurnRevoked> {
        if self.outcome.answered
            || self.transcript.tools.is_empty()
            || self.outcome.stream_interruption.is_some()
        {
            return Ok(());
        }
        self.check_harness_authorization("stopped salvage response because authorization changed")
            .await?;
        let request = self.salvage_request();
        let opened = self
            .ctx
            .ai
            .chat_stream_turn_with_services(
                request,
                temps_ai::TurnServices {
                    tools: None,
                    interactions: Some(self.ctx.interactions.clone()),
                },
            )
            .await;
        let mut stream = match opened {
            Ok(stream) => stream,
            Err(e) => {
                self.outcome.observe_error(&e);
                tracing::warn!(
                    "chat_stream_turn failed for conv {} (salvage): {e}",
                    self.ctx.conversation_id
                );
                self.outcome.remember_error(e.to_string());
                return Ok(());
            }
        };
        let salvage_text = self.consume_salvage_stream(&mut stream).await?;
        if !salvage_text.is_empty() {
            self.transcript
                .parts
                .push(serde_json::json!({ "type": "text", "text": salvage_text }));
        }
        Ok(())
    }

    /// Everything a round request carries except the tools and the MCP bridge.
    /// Takes the carried messages: nothing runs after the salvage call.
    fn salvage_request(&mut self) -> ChatTurnRequest {
        let mut messages = std::mem::take(&mut self.messages);
        messages.push(ChatMessage::user(FINAL_DIRECTIVE));
        ChatTurnRequest {
            trace_id: self.ctx.turn_id.clone(),
            purpose: format!("chat.{}.tools.final", self.ctx.context_type),
            conversation_id: Some(self.ctx.conversation_id.to_string()),
            project_id: self.ctx.project_id,
            principal_id: Some(self.ctx.authorization.auth.user_id()),
            provider: Some(self.ctx.provider_id.clone()),
            model: Some(self.ctx.model.clone()),
            thinking_level: self.ctx.thinking_level.clone(),
            permission_mode: Some(self.ctx.permission_mode.clone()),
            resume_session_id: self.session.resume_id.clone(),
            // A rebuild on the last allowed round leaves its reset for this call.
            reset_retained_session: self.session.take_reset(),
            messages,
            sandbox_attachments: self.ctx.sandbox_attachments.clone(),
            harness_workspace: self.ctx.harness_workspace.clone(),
            sandbox_environment: self.ctx.sandbox_environment.clone(),
            ..Default::default()
        }
    }

    /// Stream the salvage answer. Only its text and context usage count: it was
    /// offered no tools, so anything else it sends is not acted on.
    async fn consume_salvage_stream(
        &mut self,
        stream: &mut temps_ai::ChatTurnStream,
    ) -> Result<String, TurnRevoked> {
        let mut salvage_text = String::new();
        let mut authorization_check = tokio::time::interval(Duration::from_secs(1));
        authorization_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        authorization_check.tick().await;
        loop {
            let item = tokio::select! {
                item = stream.next() => match item {
                    Some(item) => item,
                    None => break,
                },
                _ = authorization_check.tick(), if self.ctx.harness_workspace.is_some() => {
                    match self.ctx.authorization.refresh_harness().await {
                        Ok(_) => continue,
                        Err(error) => {
                            return Err(self.revoked(
                                error,
                                "stopped active salvage harness after authorization changed",
                            ));
                        }
                    }
                }
            };
            match item {
                Ok(ChatStreamDelta::Text(t)) => {
                    if let Some(sep) = self.transcript.separator_before(&salvage_text) {
                        self.transcript.content.push_str(&sep);
                        self.ctx.events.emit(Ok(ChatStreamEvent::Token(sep)));
                    }
                    salvage_text.push_str(&t);
                    self.transcript.content.push_str(&t);
                    self.ctx.events.emit(Ok(ChatStreamEvent::Token(t)));
                }
                Ok(ChatStreamDelta::ContextUsage(usage)) => {
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
                            "failed to persist salvage context usage"
                        );
                    }
                    self.ctx
                        .events
                        .emit(Ok(ChatStreamEvent::ContextUsage(usage)));
                }
                Ok(_) => {}
                Err(e) => {
                    // The salvage answer is the turn's final text; a stream cut
                    // short here must fail the turn just like one cut short in a
                    // tool round.
                    self.outcome.observe_interruption(&e);
                    tracing::warn!(
                        "chat_stream_turn item error for conv {} (salvage): {e}",
                        self.ctx.conversation_id
                    );
                    self.outcome.remember_error(e.to_string());
                    break;
                }
            }
        }
        Ok(salvage_text)
    }
}
