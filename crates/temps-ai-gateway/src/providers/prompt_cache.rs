// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Prompt-cache breakpoints for providers that cache only on request.
//!
//! Most providers cache a repeated prompt prefix on their own. Anthropic
//! models do not: the prefix is cached only up to a `cache_control` marker, and
//! a cache read costs a tenth of fresh input. A chat resends its system prompt,
//! tool list and history on every model call, so an unmarked Claude chat pays
//! full price for tens of thousands of identical tokens each round.
//!
//! Two breakpoints are placed, well inside Anthropic's limit of four:
//!
//! * the system message, which ends the stable part (tools and system prompt);
//! * the last message that carries text, so the history that was just sent is
//!   read from cache by the next round of the same turn.

use serde_json::json;

use crate::types::{ChatMessage, ContentPart, MessageContent};

/// Whether this provider/model pair caches only behind explicit markers, and
/// accepts them. Anthropic models reached through OpenRouter do; OpenRouter
/// forwards the marker to Anthropic.
pub(crate) fn uses_explicit_prompt_cache(provider: &str, model: &str) -> bool {
    provider == "openrouter" && model.starts_with("anthropic/")
}

fn breakpoint() -> serde_json::Value {
    json!({ "type": "ephemeral" })
}

fn carries_breakpoint(message: &ChatMessage) -> bool {
    matches!(
        &message.content,
        Some(MessageContent::Parts(parts)) if parts.iter().any(|p| p.cache_control.is_some())
    )
}

/// Put the breakpoint on the message's last text part, turning a plain string
/// into a one-part list to carry it. Returns whether a marker was placed:
/// empty text cannot carry one (providers reject it) and a message with no
/// text, such as an assistant turn that only calls tools, has nowhere to put it.
fn mark(message: &mut ChatMessage) -> bool {
    match message.content.take() {
        Some(MessageContent::Text(text)) if !text.is_empty() => {
            message.content = Some(MessageContent::Parts(vec![ContentPart {
                r#type: "text".to_string(),
                text: Some(text),
                image_url: None,
                cache_control: Some(breakpoint()),
            }]));
            true
        }
        Some(MessageContent::Parts(mut parts)) => {
            let target = parts
                .iter_mut()
                .rev()
                .find(|p| p.r#type == "text" && p.text.as_deref().is_some_and(|t| !t.is_empty()));
            let placed = match target {
                Some(part) => {
                    part.cache_control = Some(breakpoint());
                    true
                }
                None => false,
            };
            message.content = Some(MessageContent::Parts(parts));
            placed
        }
        other => {
            message.content = other;
            false
        }
    }
}

/// Mark the stable prefix and the history so far as cacheable. A request the
/// caller already marked is left exactly as sent.
pub(crate) fn mark_cache_breakpoints(messages: &mut [ChatMessage]) {
    if messages.iter().any(carries_breakpoint) {
        return;
    }
    let system = messages.iter().rposition(|m| m.role == "system");
    if let Some(index) = system {
        mark(&mut messages[index]);
    }
    for index in (0..messages.len()).rev() {
        if Some(index) == system {
            // The system prompt is already a breakpoint; a second one on the
            // same block would be wasted.
            break;
        }
        if mark(&mut messages[index]) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::Text(content.to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn tool_call_only() -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            name: None,
            tool_calls: Some(vec![json!({"id": "call_1", "type": "function"})]),
            tool_call_id: None,
        }
    }

    fn marked(message: &ChatMessage) -> bool {
        carries_breakpoint(message)
    }

    #[test]
    fn only_anthropic_models_on_openrouter_use_explicit_markers() {
        assert!(uses_explicit_prompt_cache(
            "openrouter",
            "anthropic/claude-haiku-5.5"
        ));
        assert!(!uses_explicit_prompt_cache(
            "openrouter",
            "deepseek/deepseek-v4-flash-0731"
        ));
        assert!(!uses_explicit_prompt_cache("openrouter", "openai/gpt-5"));
        // Native Anthropic has its own request shape and is handled there.
        assert!(!uses_explicit_prompt_cache(
            "anthropic",
            "anthropic/claude-haiku-5.5"
        ));
    }

    #[test]
    fn system_prompt_and_latest_history_get_a_breakpoint_each() {
        let mut messages = vec![
            text("system", "You manage a deployment platform."),
            text("user", "list my projects"),
            tool_call_only(),
            text("tool", "[{\"id\":1}]"),
        ];
        mark_cache_breakpoints(&mut messages);
        assert!(marked(&messages[0]));
        assert!(!marked(&messages[1]));
        assert!(!marked(&messages[2]));
        assert!(
            marked(&messages[3]),
            "the latest tool result is the rolling breakpoint"
        );
    }

    #[test]
    fn the_rolling_breakpoint_skips_a_message_with_no_text() {
        let mut messages = vec![text("system", "s"), text("user", "q"), tool_call_only()];
        mark_cache_breakpoints(&mut messages);
        assert!(marked(&messages[1]));
        assert!(
            messages[2].content.is_none(),
            "a tool-call-only turn is left untouched"
        );
    }

    #[test]
    fn a_request_with_only_a_system_prompt_gets_one_breakpoint() {
        let mut messages = vec![text("system", "s")];
        mark_cache_breakpoints(&mut messages);
        let Some(MessageContent::Parts(parts)) = &messages[0].content else {
            panic!("system content should become parts");
        };
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].text.as_deref(), Some("s"));
        assert_eq!(parts[0].cache_control, Some(json!({"type": "ephemeral"})));
    }

    #[test]
    fn empty_text_never_carries_a_marker() {
        let mut messages = vec![text("system", ""), text("user", "")];
        mark_cache_breakpoints(&mut messages);
        assert!(!marked(&messages[0]));
        assert!(!marked(&messages[1]));
        assert_eq!(
            messages[0].content,
            Some(MessageContent::Text(String::new()))
        );
    }

    #[test]
    fn the_marker_lands_on_the_last_text_part_not_on_an_image() {
        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Parts(vec![
                ContentPart {
                    r#type: "text".to_string(),
                    text: Some("look".to_string()),
                    image_url: None,
                    cache_control: None,
                },
                ContentPart {
                    r#type: "image_url".to_string(),
                    text: None,
                    image_url: Some(json!({"url": "data:image/png;base64,AAAA"})),
                    cache_control: None,
                },
            ])),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        mark_cache_breakpoints(&mut messages);
        let Some(MessageContent::Parts(parts)) = &messages[0].content else {
            panic!("parts expected");
        };
        assert!(parts[0].cache_control.is_some());
        assert!(parts[1].cache_control.is_none());
    }

    #[test]
    fn markers_the_caller_already_placed_are_respected() {
        let mut messages = vec![text("system", "s"), text("user", "q")];
        assert!(mark(&mut messages[1]));
        let before = serde_json::to_string(&messages).unwrap_or_default();
        mark_cache_breakpoints(&mut messages);
        assert_eq!(serde_json::to_string(&messages).unwrap_or_default(), before);
        assert!(!marked(&messages[0]));
    }

    #[test]
    fn marking_is_idempotent_in_effect() {
        let mut messages = vec![text("system", "s"), text("user", "q")];
        mark_cache_breakpoints(&mut messages);
        let once = serde_json::to_string(&messages).unwrap_or_default();
        mark_cache_breakpoints(&mut messages);
        assert_eq!(serde_json::to_string(&messages).unwrap_or_default(), once);
    }
}
