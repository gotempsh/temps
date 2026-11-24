// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The live wire form of chat stream events, shared by SSE and WebSocket viewers.

use super::*;

/// One event on a conversation's live wire. The server-owned producer derives
/// this and the detachable SSE item from the same normalized event.
#[derive(Debug, Clone)]
pub struct WireEvent {
    /// SSE/WS event name, e.g. `"token"` (implicit/unnamed for plain text in
    /// SSE), `"tool_call"`, `"permission_requested"`, `"user_message"`.
    pub event: String,
    /// The JSON (or plain text, for token deltas) payload.
    pub data: String,
}

/// Convert one normalized provider event into the shared live-wire contract.
/// The producer publishes here, before attempting delivery to any particular
/// SSE viewer, so closing or refreshing a browser cannot silence other tabs.
pub(super) fn wire_event_for(item: &Result<ChatStreamEvent, ChatError>) -> WireEvent {
    match item {
        Ok(ChatStreamEvent::Token(text)) => WireEvent {
            event: "token".to_string(),
            data: text.clone(),
        },
        Ok(ChatStreamEvent::ContextUsage(usage)) => WireEvent {
            event: "context_usage".to_string(),
            data: serde_json::to_string(usage).unwrap_or_else(|_| "{}".to_string()),
        },
        Ok(ChatStreamEvent::ToolCall {
            id,
            name,
            arguments,
        }) => WireEvent {
            event: "tool_call".to_string(),
            data: serde_json::json!({
                "id": id,
                "name": name,
                "arguments": arguments,
            })
            .to_string(),
        },
        Ok(ChatStreamEvent::ToolResult { id, name, content }) => WireEvent {
            event: "tool_result".to_string(),
            data: serde_json::json!({
                "id": id,
                "name": name,
                "content": content,
            })
            .to_string(),
        },
        Ok(ChatStreamEvent::PermissionRequested {
            id,
            kind,
            tool_name,
            input,
        }) => WireEvent {
            event: "permission_requested".to_string(),
            data: serde_json::json!({
                "id": id,
                "kind": kind,
                "tool_name": tool_name,
                "input": input,
            })
            .to_string(),
        },
        Err(error) => WireEvent {
            event: "error".to_string(),
            data: {
                let failure = error.public_failure();
                serde_json::json!({
                    "code": failure.code,
                    "title": failure.title,
                    "detail": failure.detail,
                    "retryable": failure.retryable,
                })
                .to_string()
            },
        },
    }
}

pub(super) fn emit_turn_event(
    tx: &tokio::sync::mpsc::UnboundedSender<Result<ChatStreamEvent, ChatError>>,
    live: &broadcast::Sender<WireEvent>,
    item: Result<ChatStreamEvent, ChatError>,
) {
    let _ = live.send(wire_event_for(&item));
    // Zero SSE viewers is normal after a refresh. Execution and WebSocket
    // publication remain server-owned and continue until terminal state.
    let _ = tx.send(item);
}
