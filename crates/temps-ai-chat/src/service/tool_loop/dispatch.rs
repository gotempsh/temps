// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running the tool calls a round produced, for the model's next round. A
//! sandbox harness runs its own tools; these are the ones it left to us.

use super::*;

impl ToolLoop {
    /// Run the calls in order and carry each result to the next round. Returns
    /// whether any of them produced usable data rather than a rejection.
    pub(super) async fn dispatch_round_calls(
        &mut self,
        calls: &[ToolCall],
    ) -> Result<bool, TurnRevoked> {
        let mut produced_something = false;
        for (call_index, call) in calls.iter().enumerate() {
            let result = self.run_tool_call(call_index, call).await?;
            self.publish_tool_result(call, &result).await;
            produced_something |= tool_result_is_productive(&result);
            self.messages
                .push(ChatMessage::tool(call.id.clone(), result));
        }
        Ok(produced_something)
    }

    async fn run_tool_call(
        &self,
        call_index: usize,
        call: &ToolCall,
    ) -> Result<String, TurnRevoked> {
        if call_index >= self.budget.max_calls_per_round() {
            // Answer the call rather than dropping it: a provider rejects an
            // assistant tool_calls block with unanswered ids, and the model needs
            // to know the call did not run.
            return Ok(tool_call_cap_message(self.budget.max_calls_per_round()));
        }
        match self.ctx.authorization.refresh_tool().await {
            Ok(fresh_auth) => Ok(self.router.dispatch(call, &fresh_auth).await),
            Err(error) => {
                tracing::warn!(
                    conversation_id = self.ctx.conversation_id,
                    tool = %call.name,
                    %error,
                    "denied conversation tool after authorization refresh failed"
                );
                self.ctx
                    .events
                    .emit(Err(ChatError::AuthorizationRefresh(error)));
                Err(TurnRevoked)
            }
        }
    }

    /// Surface the result right after the call, live, then record it.
    async fn publish_tool_result(&mut self, call: &ToolCall, result: &str) {
        let display_arguments = redact_json_string(&call.arguments);
        let display_result = public_tool_result(&call.name, result);
        self.ctx.events.emit(Ok(ChatStreamEvent::ToolResult {
            id: call.id.clone(),
            name: call.name.clone(),
            content: display_result.clone(),
        }));
        self.transcript
            .record_result(&call.id, &call.name, &display_arguments, &display_result);
        self.checkpoint("dispatched tool result").await;
    }
}
