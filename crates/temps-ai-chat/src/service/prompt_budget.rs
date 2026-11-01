// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-turn budget: tool rounds and calls per round, and how much history
//! and tool output is replayed to the model.

use super::*;

/// Tool-call rounds one assistant turn may take. Far below the generic
/// anti-spin ceiling because the assistant is a conversational surface with a
/// user watching, but enough for one accepted design to go from an empty
/// sandbox to a deployment: writing the files, installing, building, testing,
/// creating and binding the project and its services, deploying and watching
/// it took a little under 30 rounds in practice. The wall-clock deadline and
/// the unproductive-round stop still apply on top; this is the step backstop.
pub(super) const ASSISTANT_MAX_ROUNDS: usize = 40;
/// Rounds before the step cap at which the model is told to wrap up.
const WRAP_UP_ROUNDS: usize = 3;
/// What the model reads when [`WRAP_UP_ROUNDS`] rounds are left.
pub(super) const STEPS_LEFT_NOTE: &str =
    "\n\n[Temps: this turn has 3 tool rounds left. Start nothing new. \
If a deployment, build or other job you started is still running, call `temps_goal` now to \
watch it, so the chat comes back to it when it finishes; then reply with what is done, what is \
still running and what comes next.]";

/// The wrap-up note to add after round `rounds_done`, for capped turns only:
/// the generic ceiling is an anti-spin guard, not a budget the model plans by.
pub(super) fn steps_left_note(max_rounds: usize, rounds_done: usize) -> Option<&'static str> {
    (max_rounds <= ASSISTANT_MAX_ROUNDS
        && max_rounds > WRAP_UP_ROUNDS
        && rounds_done + WRAP_UP_ROUNDS == max_rounds)
        .then_some(STEPS_LEFT_NOTE)
}
/// Tool calls honoured from a single model response. Models can emit dozens of
/// parallel calls; each is a replayed API request plus a transcript entry.
pub(super) const ASSISTANT_MAX_CALLS_PER_ROUND: usize = 8;
/// Largest transcript, in bytes of message content, replayed to the model on
/// a new turn of any conversation. Summarizing (`compaction`) normally keeps a
/// chat well under this; it is the backstop when no summary could be written.
pub(super) const CHAT_MAX_HISTORY_BYTES: usize = 128 * 1024;
/// Marker the over-cap result starts with; also registered as an unproductive
/// result so a model that keeps exceeding the cap is stopped.
pub(super) const TOOL_CALL_CAP_REJECTION: &str = "Not executed:";

pub(super) fn max_rounds_for_context(context_type: &str, default: usize) -> usize {
    if context_type == "assistant" {
        ASSISTANT_MAX_ROUNDS.min(default)
    } else {
        default
    }
}

pub(super) fn max_calls_per_round_for_context(context_type: &str) -> usize {
    if context_type == "assistant" {
        ASSISTANT_MAX_CALLS_PER_ROUND
    } else {
        usize::MAX
    }
}

pub(super) fn tool_call_cap_message(cap: usize) -> String {
    format!(
        "{TOOL_CALL_CAP_REJECTION} at most {cap} tool calls run per step. Make fewer, more \
         targeted calls (or one call per step) and repeat this one if it is still needed."
    )
}

/// Where a turn's prompt bytes go, so the fixed part (system framing and tool
/// schemas, resent on every model call) can be told apart from the part that
/// grows with the conversation. Bytes, not tokens: tokenisers differ per model,
/// and the ratio between the three is what matters.
pub(super) fn log_prompt_composition(
    conv: &temps_entities::ai_conversations::Model,
    messages: &[ChatMessage],
    tools: &[ChatTool],
) {
    let system_bytes: usize = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.len())
        .sum();
    let history_bytes: usize = messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| m.content.len())
        .sum();
    let tool_schema_bytes: usize = tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.parameters.to_string().len())
        .sum();
    tracing::debug!(
        conversation_id = conv.id,
        sections = %system_section_sizes(messages),
        "system prompt sections"
    );
    tracing::info!(
        conversation_id = conv.id,
        context_type = %conv.context_type,
        system_bytes,
        tool_schema_bytes,
        tool_count = tools.len(),
        history_bytes,
        history_messages = messages.iter().filter(|m| m.role != "system").count(),
        "assembled chat prompt"
    );
}

/// The system prompt's size by markdown heading, largest first, as
/// `heading=bytes` pairs. Shows which part of the fixed prefix is worth
/// shrinking without printing the prompt itself.
pub(super) fn system_section_sizes(messages: &[ChatMessage]) -> String {
    let mut sections: Vec<(String, usize)> = Vec::new();
    for message in messages.iter().filter(|m| m.role == "system") {
        for line in message.content.split_inclusive('\n') {
            if line.starts_with('#') {
                let title: String = line
                    .trim_start_matches('#')
                    .trim()
                    .chars()
                    .take(48)
                    .collect();
                sections.push((title, 0));
            } else if let Some(last) = sections.last_mut() {
                last.1 += line.len();
            } else {
                sections.push(("(preamble)".to_string(), line.len()));
            }
        }
    }
    sections.sort_by_key(|section| std::cmp::Reverse(section.1));
    sections
        .iter()
        .take(12)
        .map(|(title, bytes)| format!("{title}={bytes}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Keep a long conversation inside the model's context by dropping its oldest
/// turns. System messages and the most recent message always survive, and a
/// note appended to the system message marks the gap so the model does not
/// mistake the remainder for the whole conversation. Returns the number of
/// messages dropped.
///
/// Only whole messages are removed, oldest first, so no tool-call/result pair
/// is ever split (history loaded from storage holds only system, user and
/// assistant text).
pub(super) fn bound_history_for_model(messages: &mut Vec<ChatMessage>, budget: usize) -> usize {
    let total: usize = messages.iter().map(|m| m.content.len()).sum();
    if total <= budget || messages.len() < 2 {
        return 0;
    }
    let last = messages.len() - 1;
    let mut over = total - budget;
    let mut drop = vec![false; messages.len()];
    let mut dropped = 0usize;
    for (index, message) in messages.iter().enumerate() {
        if over == 0 {
            break;
        }
        if index == last || message.role == "system" {
            continue;
        }
        over = over.saturating_sub(message.content.len());
        drop[index] = true;
        dropped += 1;
    }
    if dropped == 0 {
        return 0;
    }
    let mut index = 0;
    messages.retain(|_| {
        let keep = !drop[index];
        index += 1;
        keep
    });
    let note = format!(
        "\n\n[{dropped} earlier message(s) of this conversation were omitted to stay within the context budget. Ask the user, or re-run a read, if you need something from them.]"
    );
    match messages.iter_mut().find(|m| m.role == "system") {
        Some(system) => system.content.push_str(&note),
        None => messages.insert(0, ChatMessage::system(note.trim_start().to_string())),
    }
    dropped
}

/// Replace the oldest tool results with a stub once the carried-forward
/// transcript grows past `budget` bytes, and keep stubbing until it is down to
/// half of that.
///
/// Every tool result is replayed to the model on every subsequent round, so a
/// long task re-sends everything it has ever read — the transcript, not the
/// round count, is what actually bounds how long a turn can run. Without this,
/// raising the round cap just moves the failure from "stopped early" to "blew
/// the context window", which is worse because it is not explained.
///
/// Trimming down to half the budget, rather than to just under it, is what
/// keeps the prompt cache useful: a provider caches the prompt up to the first
/// byte that differs from the previous request, so trimming a little on every
/// round would rewrite the middle of the prompt every round, while trimming a
/// lot once leaves the next several rounds with an identical prefix.
///
/// The messages are kept in place with their `tool_call_id` intact — providers
/// reject a tool_calls block whose answers have gone missing — and only the
/// content is replaced, so the model still sees that the call happened and what
/// it was. The results of the latest round, the ones the model is answering
/// right now, are never stubbed.
pub(super) fn trim_carried_tool_results(messages: &mut [ChatMessage], budget: usize) {
    const STUB: &str = "[earlier tool result dropped to stay within the context budget — re-run the command if you still need it]";
    let total: usize = messages
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.content.len())
        .sum();
    if total <= budget {
        return;
    }

    // The trailing run of tool messages is the latest round's results.
    let protected_from = messages
        .iter()
        .rposition(|m| m.role != "tool")
        .map_or(0, |index| index + 1);

    // Drop oldest-first: recent results are what the model is reasoning about
    // right now, and the older ones have usually been summarised into its own
    // prose already.
    let mut over = total - budget / 2;
    for m in messages[..protected_from]
        .iter_mut()
        .filter(|m| m.role == "tool")
    {
        if over == 0 {
            break;
        }
        if m.content.len() <= STUB.len() {
            continue;
        }
        over = over.saturating_sub(m.content.len() - STUB.len());
        m.content = STUB.to_string();
    }
}
