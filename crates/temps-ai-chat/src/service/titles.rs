// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conversation titles: generating, refreshing, cleaning and storing them.

use super::*;

/// System prompt for the one-shot title generator. Kept terse so even small
/// local models return a clean label rather than a sentence.
const TITLE_SYSTEM_PROMPT: &str = "You write a short title for a chat based on the user's first message. \
Reply with ONLY the title: 3–6 words, Title Case, no quotes, no surrounding punctuation, no explanation.";

/// System prompt for re-titling once there is a conversation to summarize.
/// The first message is often a vague opener ("hi", "can you help me"), so the
/// refreshed title is written from what the chat actually turned out to be about.
const TITLE_FROM_CONVERSATION_SYSTEM_PROMPT: &str = "You write a short title for a chat by summarizing the whole conversation. \
Name the goal or outcome the user is working toward, not the wording of their first message. \
Reply with ONLY the title: 3–6 words, Title Case, no quotes, no surrounding punctuation, no explanation.";

/// User turns after which the title is rewritten from the conversation so far.
/// Early enough that the list is useful, sparse enough to cost three small
/// completions over a long chat.
const TITLE_REFRESH_USER_TURNS: [usize; 3] = [1, 3, 6];

/// Most characters of each message, and of the whole digest, fed to the title
/// model. Titles need the gist, not the transcript.
pub(super) const TITLE_DIGEST_MESSAGE_CHARS: usize = 600;
pub(super) const TITLE_DIGEST_TOTAL_CHARS: usize = 5000;

/// `ai_conversations.metadata` key recording who last set the title:
/// `"auto"` (generated) or `"user"` (renamed). Generated titles never replace a
/// name the user chose.
const TITLE_SOURCE_KEY: &str = "title_source";

/// Maximum stored title length (chars). Long titles are truncated, not rejected.
pub(super) const TITLE_MAX_CHARS: usize = 60;

/// Normalise a model-generated title: take the first non-empty line, strip
/// wrapping quotes and trailing punctuation, collapse whitespace, and cap the
/// length. Reasoning models sometimes prepend stray lines, so we defensively
/// keep only the first meaningful one.
pub(super) fn clean_title(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim_end_matches(['.', '!', '?', ':', ',', ';'])
        .trim();
    if trimmed.chars().count() > TITLE_MAX_CHARS {
        trimmed
            .chars()
            .take(TITLE_MAX_CHARS)
            .collect::<String>()
            .trim_end()
            .to_string()
    } else {
        trimmed.to_string()
    }
}

/// Ask the AI for a concise title for `first_message` and store it on the
/// conversation. Fire-and-forget: every failure (AI unavailable, empty result,
/// DB error) is swallowed with a debug log so it can never break the chat.
///
/// Uses [`AiService::complete`] rather than `chat()` — it needs no tools, and
/// `complete()` is the workload [`temps_ai_agent_cli::DispatchingAiService`]
/// routes to whichever provider is active, so a subscription agent CLI still
/// gets a real title instead of always falling back to the generic seed
/// title (`chat()` stays gateway-only unconditionally; using it here would
/// silently fail for every CLI-backed instance).
pub(super) async fn generate_and_store_title(
    ai: &Arc<dyn AiService>,
    db: &Arc<DatabaseConnection>,
    conv_id: i64,
    project_id: Option<i32>,
    provider: String,
    model: String,
    first_message: &str,
) {
    let Some(title) = complete_title(
        ai,
        conv_id,
        project_id,
        provider,
        model,
        TITLE_SYSTEM_PROMPT,
        format!("First message:\n{first_message}\n\nTitle:"),
    )
    .await
    else {
        return;
    };
    store_generated_title(db, conv_id, title).await;
}

/// Rewrite the title from the conversation so far, at the turn counts in
/// [`TITLE_REFRESH_USER_TURNS`]. Same fire-and-forget contract as
/// [`generate_and_store_title`]; additionally a no-op once the user has renamed
/// the chat.
pub(super) async fn refresh_title_from_conversation(
    ai: &Arc<dyn AiService>,
    db: &Arc<DatabaseConnection>,
    conv_id: i64,
    project_id: Option<i32>,
    provider: String,
    model: String,
) {
    let current = match ai_conversations::Entity::find_by_id(conv_id)
        .one(db.as_ref())
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!("title refresh could not load conv {conv_id}: {e}");
            return;
        }
    };
    if title_was_set_by_user(current.metadata.as_ref()) {
        return;
    }
    let messages = match ai_messages::Entity::find()
        .filter(ai_messages::Column::ConversationId.eq(conv_id))
        .order_by_asc(ai_messages::Column::Id)
        .all(db.as_ref())
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!("title refresh could not load messages of conv {conv_id}: {e}");
            return;
        }
    };
    let user_turns = messages.iter().filter(|m| m.role == "user").count();
    if !TITLE_REFRESH_USER_TURNS.contains(&user_turns) {
        return;
    }
    let digest = conversation_digest(
        messages
            .iter()
            .map(|m| (m.role.as_str(), m.content.as_str())),
    );
    if digest.is_empty() {
        return;
    }
    let Some(title) = complete_title(
        ai,
        conv_id,
        project_id,
        provider,
        model,
        TITLE_FROM_CONVERSATION_SYSTEM_PROMPT,
        format!("Conversation:\n{digest}\n\nTitle:"),
    )
    .await
    else {
        return;
    };
    // The user may have renamed the chat while the model was thinking.
    match ai_conversations::Entity::find_by_id(conv_id)
        .one(db.as_ref())
        .await
    {
        Ok(Some(row)) if !title_was_set_by_user(row.metadata.as_ref()) => {
            store_generated_title(db, conv_id, title).await;
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("title refresh could not re-check conv {conv_id}: {e}"),
    }
}

async fn complete_title(
    ai: &Arc<dyn AiService>,
    conv_id: i64,
    project_id: Option<i32>,
    provider: String,
    model: String,
    system: &str,
    prompt: String,
) -> Option<String> {
    let req = AiRequest {
        purpose: "chat.title".to_string(),
        project_id,
        provider: Some(provider),
        model: Some(model),
        system: Some(system.to_string()),
        prompt,
        ..Default::default()
    };
    let raw = match ai.complete(req).await {
        Ok(resp) => resp.text,
        Err(e) => {
            tracing::debug!("chat title generation failed for conv {conv_id}: {e}");
            return None;
        }
    };
    let title = clean_title(&raw);
    if title.is_empty() {
        tracing::debug!("chat title generation produced an empty title for conv {conv_id}");
        return None;
    }
    Some(title)
}

async fn store_generated_title(db: &Arc<DatabaseConnection>, conv_id: i64, title: String) {
    let existing = match ai_conversations::Entity::find_by_id(conv_id)
        .one(db.as_ref())
        .await
    {
        Ok(Some(row)) => row.metadata,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!("failed to read metadata before titling conv {conv_id}: {e}");
            return;
        }
    };
    let am = ai_conversations::ActiveModel {
        id: Set(conv_id),
        title: Set(Some(title)),
        metadata: Set(Some(with_title_source(existing, "auto"))),
        ..Default::default()
    };
    if let Err(e) = am.update(db.as_ref()).await {
        tracing::debug!("failed to store generated title for conv {conv_id}: {e}");
    }
}

pub(super) fn title_was_set_by_user(metadata: Option<&serde_json::Value>) -> bool {
    metadata
        .and_then(|m| m.get(TITLE_SOURCE_KEY))
        .and_then(serde_json::Value::as_str)
        == Some("user")
}

/// `metadata` with [`TITLE_SOURCE_KEY`] set, keeping every other key. A
/// non-object value is replaced: it carries nothing this code understands.
pub(super) fn with_title_source(
    metadata: Option<serde_json::Value>,
    source: &str,
) -> serde_json::Value {
    let mut object = match metadata {
        Some(serde_json::Value::Object(object)) => object,
        _ => serde_json::Map::new(),
    };
    object.insert(
        TITLE_SOURCE_KEY.to_string(),
        serde_json::Value::String(source.to_string()),
    );
    serde_json::Value::Object(object)
}

/// A compact, bounded transcript of the user/assistant text for the title
/// model. The newest messages win the budget: that is where the chat's actual
/// subject has usually settled.
pub(super) fn conversation_digest<'a>(
    messages: impl Iterator<Item = (&'a str, &'a str)>,
) -> String {
    let lines: Vec<String> = messages
        .filter(|(role, content)| {
            matches!(*role, "user" | "assistant") && !content.trim().is_empty()
        })
        .map(|(role, content)| {
            let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
            let clipped: String = flat.chars().take(TITLE_DIGEST_MESSAGE_CHARS).collect();
            let speaker = if role == "user" { "User" } else { "Assistant" };
            format!("{speaker}: {clipped}")
        })
        .collect();
    let mut kept: Vec<&String> = Vec::new();
    let mut budget = TITLE_DIGEST_TOTAL_CHARS;
    for line in lines.iter().rev() {
        let cost = line.chars().count() + 1;
        if cost > budget && !kept.is_empty() {
            break;
        }
        budget = budget.saturating_sub(cost);
        kept.push(line);
    }
    kept.reverse();
    kept.into_iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}
