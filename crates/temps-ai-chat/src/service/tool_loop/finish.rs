// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The end of a turn: decide whether it failed, tell the client, and persist
//! what is left to keep.

use super::*;

impl ToolLoop {
    /// Settle the turn and persist it. Returns whether the turn failed.
    pub(super) async fn finish(&mut self) -> bool {
        let proposal_not_staged = self.correct_unbacked_proposal_claim();

        // A turn that produced nothing at all, no prose and no tool work, is
        // indistinguishable from a hung request in the UI: the user's own message
        // sits there and nothing ever arrives. That is the worst possible outcome
        // for a self-hosted operator with no support channel to ask, so say what
        // went wrong. The client already renders an `error` SSE event; before this
        // it simply never received one, because a provider failure just ended the
        // loop.
        let empty_turn_failed = self.transcript.is_empty();
        let turn_failed = proposal_not_staged
            || empty_turn_failed
            || self.outcome.retained_diagnostic.is_some()
            || self.outcome.stream_interruption.is_some();
        let provider_stopped_early =
            self.outcome.last_provider_error.is_some() || self.outcome.stop_reason.is_some();

        let interrupted_message = self.report_interruption();
        self.report_failure(empty_turn_failed, interrupted_message.is_some());
        if turn_failed || provider_stopped_early {
            self.close_unresolved_tool_calls();
        }

        let response_bytes = self.transcript.content.len();
        let tool_count = self.transcript.tools.len();
        self.persist_session_id().await;
        self.persist_session_title().await;
        self.persist_message(interrupted_message.as_ref()).await;
        self.persist_failure().await;
        tracing::info!(
            component = "ai_turn_timing",
            turn_id = self.ctx.trace_id(),
            conversation_id = self.ctx.conversation_id,
            project_id = self.ctx.project_id,
            provider = %self.ctx.provider_id,
            phase = "execution_task_complete",
            total_ms = self.budget.elapsed().as_millis() as u64,
            response_bytes,
            tool_count,
            failed = turn_failed,
            "AI turn timing"
        );
        turn_failed
    }

    /// The write contract is receipt-backed. A model can see an older proposal
    /// in conversation history and incorrectly describe it as a new one without
    /// calling the tool again. Never persist that claim as success: there is no
    /// durable action for the UI to render or confirm.
    fn correct_unbacked_proposal_claim(&mut self) -> bool {
        if !claims_proposal_was_staged(&self.transcript.content)
            || has_fresh_proposal_receipt(&self.transcript.tools)
        {
            return false;
        }
        tracing::warn!(
            conv_id = self.ctx.conversation_id,
            "assistant claimed a proposal was staged without a current-turn receipt"
        );
        const CORRECTION: &str = "Temps could not verify this proposal because the AI did not submit it through the write tool. No approval card was created and no change was made. Retry to create a fresh proposal.";
        self.transcript.replace_prose(CORRECTION);
        self.ctx.events.emit(Err(ChatError::ProposalNotStaged));
        self.outcome.durable_failure = Some(ChatError::ProposalNotStaged.public_failure());
        true
    }

    /// A reply the provider never finished is a failed turn even when text
    /// arrived: persisting that text as a finished answer is how a truncated
    /// reply used to be shown with no error. The text is kept, flagged as
    /// partial, and the failure is both emitted and persisted.
    fn report_interruption(&mut self) -> Option<crate::PublicChatFailure> {
        let cause = self.outcome.stream_interruption.take()?;
        let trace_id = self.ctx.trace_id();
        let error = ChatError::StreamInterrupted {
            conversation_id: self.ctx.conversation_id,
            turn_id: trace_id.to_string(),
            partial_bytes: self.transcript.content.len(),
            cause,
        };
        let failure = error.public_failure();
        tracing::warn!(
            component = "ai_turn_timing",
            turn_id = trace_id,
            conversation_id = self.ctx.conversation_id,
            failed = true,
            %error,
            "AI turn failed: provider stream interrupted"
        );
        self.ctx.events.emit(Err(error));
        self.outcome.durable_failure.get_or_insert(failure.clone());
        Some(failure)
    }

    fn report_failure(&mut self, empty_turn_failed: bool, already_interrupted: bool) {
        if empty_turn_failed && already_interrupted {
            // Already reported with the precise cause; do not follow it with the
            // generic "no reply was produced" error.
            return;
        }
        let retained = self
            .outcome
            .retained_diagnostic
            .take()
            .map(|(purpose, reason)| ChatError::RetainedHarnessDiagnostic { purpose, reason });
        let error = match (empty_turn_failed, retained) {
            (_, Some(retained)) => retained,
            (true, None) => {
                // `ChatError::Ai` already renders an "AI provider error: " prefix,
                // so the message continues that sentence rather than restating it.
                let detail = self
                    .outcome
                    .last_provider_error
                    .take()
                    .unwrap_or_else(|| "the provider returned no response".to_string());
                ChatError::Ai(format!(
                    "no reply was produced. {detail} \
                     Review the concrete error and the selected harness/provider status, \
                     then try again."
                ))
            }
            (false, None) => return,
        };
        self.outcome.durable_failure = Some(error.public_failure());
        self.ctx.events.emit(Err(error));
    }

    fn close_unresolved_tool_calls(&mut self) {
        let unresolved =
            finish_unresolved_tool_calls(&mut self.transcript.tools, &mut self.transcript.parts);
        for (id, name) in unresolved {
            self.ctx.events.emit(Ok(ChatStreamEvent::ToolResult {
                id,
                name,
                content: INTERRUPTED_TOOL_RESULT.to_string(),
            }));
        }
    }

    async fn persist_session_id(&mut self) {
        let Some(session_id) = self.session.reported_id.take() else {
            return;
        };
        if let Err(error) = ai_conversations::Entity::update_many()
            .filter(ai_conversations::Column::Id.eq(self.ctx.conversation_id))
            .col_expr(
                ai_conversations::Column::CliSessionId,
                Expr::value(Some(session_id)),
            )
            .exec(self.ctx.db.as_ref())
            .await
        {
            tracing::warn!(
                conversation_id = self.ctx.conversation_id,
                %error,
                "failed to persist harness session id"
            );
        }
    }

    async fn persist_session_title(&self) {
        if !self.ctx.capture_session_title || self.ctx.context_type != "application" {
            return;
        }
        let Some(title) = self
            .session
            .reported_title
            .as_deref()
            .map(clean_title)
            .filter(|title| !title.is_empty())
        else {
            return;
        };
        // Do not overwrite a manual rename made while the first turn was
        // running: replace only the exact seed title the conversation had when
        // this request started.
        let conversation_id = self.ctx.conversation_id;
        let condition = match self.ctx.initial_title.as_deref() {
            Some(initial) => Condition::all()
                .add(ai_conversations::Column::Id.eq(conversation_id))
                .add(ai_conversations::Column::Title.eq(initial.to_string())),
            None => Condition::all()
                .add(ai_conversations::Column::Id.eq(conversation_id))
                .add(ai_conversations::Column::Title.is_null()),
        };
        match ai_conversations::Entity::update_many()
            .filter(condition)
            .col_expr(
                ai_conversations::Column::Title,
                Expr::value(Some(title.clone())),
            )
            .exec(self.ctx.db.as_ref())
            .await
        {
            Ok(result) if result.rows_affected == 1 => {
                let _ = self.ctx.events.live.send(WireEvent {
                    event: "conversation_title".to_string(),
                    data: serde_json::json!({ "title": title }).to_string(),
                });
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(
                conversation_id,
                %error,
                "failed to store harness-provided conversation title"
            ),
        }
    }

    /// Persist the assistant turn once complete. `content` is the full prose for
    /// history replay; `metadata.tools` + `metadata.parts` let the UI replay the
    /// tool work and interleaving on reload. Skip an entirely empty turn.
    async fn persist_message(&self, interrupted: Option<&crate::PublicChatFailure>) {
        if self.transcript.is_empty() {
            self.remove_empty_draft().await;
            return;
        }
        let Some(message_id) = self.store_final_message(interrupted).await else {
            return;
        };
        // Best-effort: link any pending actions created during this turn to the
        // persisted assistant message so the UI can correlate them.
        let proposed_action_ids = self.router.state.lock().await.proposed_action_ids.clone();
        if proposed_action_ids.is_empty() {
            return;
        }
        if let Some(pending) = &self.router.pending {
            if let Err(e) = pending.link_message(&proposed_action_ids, message_id).await {
                tracing::warn!(
                    conv_id = self.ctx.conversation_id,
                    "Failed to link pending actions to message {message_id}: {e}"
                );
            }
        }
    }

    async fn store_final_message(
        &self,
        interrupted: Option<&crate::PublicChatFailure>,
    ) -> Option<i64> {
        let conv_id = self.ctx.conversation_id;
        let Some(message_id) = self.ctx.draft_message_id else {
            let am = ai_messages::ActiveModel {
                conversation_id: Set(conv_id),
                role: Set("assistant".to_string()),
                content: Set(self.transcript.content.clone()),
                metadata: Set(assistant_message_metadata_with(
                    &self.transcript.tools,
                    &self.transcript.parts,
                    false,
                    interrupted,
                )),
                created_at: Set(Utc::now()),
                ..Default::default()
            };
            return am
                .insert(self.ctx.db.as_ref())
                .await
                .ok()
                .map(|message| message.id);
        };
        match persist_assistant_message_with(
            self.ctx.db.as_ref(),
            message_id,
            &self.transcript.content,
            &self.transcript.tools,
            &self.transcript.parts,
            "",
            false,
            interrupted,
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

    /// Preserve the previous behavior for a provider that produced no assistant
    /// output at all: the structured failure event is authoritative, not an empty
    /// assistant bubble.
    async fn remove_empty_draft(&self) {
        let Some(message_id) = self.ctx.draft_message_id else {
            return;
        };
        if let Err(error) = ai_messages::Entity::delete_by_id(message_id)
            .exec(self.ctx.db.as_ref())
            .await
        {
            tracing::warn!(
                conv_id = self.ctx.conversation_id,
                message_id,
                %error,
                "failed to remove empty assistant draft"
            );
        }
    }

    async fn persist_failure(&self) {
        let Some(failure) = self.outcome.durable_failure.as_ref() else {
            return;
        };
        if let Err(error) = persist_conversation_failure(
            self.ctx.db.as_ref(),
            self.ctx.conversation_id,
            self.ctx.turn_id.as_deref(),
            failure,
        )
        .await
        {
            tracing::error!(
                conversation_id = self.ctx.conversation_id,
                %error,
                "failed to persist safe AI turn failure"
            );
        }
    }
}
