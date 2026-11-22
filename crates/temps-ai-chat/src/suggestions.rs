// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Next-step suggestions: a few short follow-ups offered after an answer.
//!
//! Whether they appear is the user's choice, at two levels: a default per user
//! (`ai_user_preferences`) and an override per chat
//! (`ai_conversations.next_step_suggestions`). The mode is `off`, `ask` (the
//! console offers them after each answer and spends nothing until the user says
//! yes) or `always`.
//!
//! Suggestions cost a request of their own, so it is kept small: the model is
//! sent only the latest question and answer, clipped, never the whole chat.
//! What it costs is logged against the chat like every other request, so the
//! chat's spend includes it.
//!
//! The server, not the console, decides when that request is made, so no client
//! can spend more than one request per answer:
//! - suggestions are saved on the answer they follow, and asking again for the
//!   same answer returns the saved set without calling the model;
//! - one request per chat runs at a time;
//! - nothing is generated while a turn is running, for an answer that is no
//!   longer the latest, or in a chat where suggestions are off.

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use serde::{Deserialize, Serialize};
use temps_ai::{AiService, ChatMessage, ChatStreamDelta, ChatTurnRequest};
use temps_entities::{ai_conversations, ai_messages, ai_user_preferences};
use thiserror::Error;
use utoipa::ToSchema;

use crate::providers::assistant::ASSISTANT_CONTEXT_TYPE;
use crate::service::encode_message_before_cursor;

/// Key, in an answer's metadata, of the suggestions saved for it.
const SAVED_KEY: &str = "next_steps";

/// Suggestions offered after one answer.
pub const MAX_SUGGESTIONS: usize = 3;
/// Longest suggestion kept; a longer line is cut at a word.
const MAX_SUGGESTION_CHARS: usize = 120;
/// Longest slice of the question and of the answer sent to the model.
const MAX_QUESTION_BYTES: usize = 2 * 1024;
const MAX_ANSWER_BYTES: usize = 6 * 1024;
/// Recent rows read to find the latest exchange.
const RECENT_ROWS: u64 = 12;
const MAX_OUTPUT_TOKENS: u32 = 300;
const MODEL_TIMEOUT: Duration = Duration::from_secs(30);

const SUGGESTIONS_SYSTEM_PROMPT: &str = "You suggest what the user of the Temps console assistant might ask next. \
Read the user's last message and the assistant's answer. Reply with exactly three lines and nothing else: \
each line is one next step, written as the user's own short request to the assistant (under 80 characters), \
in the language of the user's message. Make them concrete and different from each other, follow on from the answer, \
and never include a password, key or token. If the answer was cut off before it finished, make one of them a request to carry on. \
No numbering, bullets or quotes.";

/// What happens after each answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionMode {
    /// Never offered.
    Off,
    /// The console asks after each answer; nothing is spent until the user says yes.
    Ask,
    /// Suggested after every answer.
    Always,
}

impl SuggestionMode {
    pub const DEFAULT: SuggestionMode = SuggestionMode::Ask;

    pub fn as_str(self) -> &'static str {
        match self {
            SuggestionMode::Off => "off",
            SuggestionMode::Ask => "ask",
            SuggestionMode::Always => "always",
        }
    }
}

impl FromStr for SuggestionMode {
    type Err = SuggestionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(SuggestionMode::Off),
            "ask" => Ok(SuggestionMode::Ask),
            "always" => Ok(SuggestionMode::Always),
            other => Err(SuggestionError::InvalidMode {
                value: other.to_string(),
            }),
        }
    }
}

/// Where a chat's effective mode comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionModeSource {
    /// The chat sets its own mode.
    Chat,
    /// The chat follows the user's default.
    Default,
}

#[derive(Debug, Error)]
pub enum SuggestionError {
    #[error("'{value}' is not a suggestion mode; use 'off', 'ask' or 'always'")]
    InvalidMode { value: String },
    #[error("could not {operation} for user {user_id}: {source}")]
    Preferences {
        operation: &'static str,
        user_id: i32,
        #[source]
        source: DbErr,
    },
    #[error("could not {operation} for conversation {conversation_id}: {source}")]
    Conversation {
        operation: &'static str,
        conversation_id: i64,
        #[source]
        source: DbErr,
    },
    #[error(
        "conversation {conversation_id} is a '{context_type}' chat; next-step suggestions are offered only in the assistant chat"
    )]
    Unavailable {
        conversation_id: i64,
        context_type: String,
    },
    #[error("conversation {conversation_id} has no finished answer to suggest next steps for")]
    NoAnswer { conversation_id: i64 },
    #[error(
        "model '{model}' failed while suggesting next steps for conversation {conversation_id}: {reason}"
    )]
    Model {
        conversation_id: i64,
        model: String,
        reason: String,
    },
    #[error("model '{model}' returned no usable suggestion for conversation {conversation_id}")]
    Empty { conversation_id: i64, model: String },
    #[error(
        "next-step suggestions are off for conversation {conversation_id}; turn them on for this chat or in your default first"
    )]
    Off { conversation_id: i64 },
    #[error(
        "conversation {conversation_id} is still answering; next steps can be suggested once the turn ends"
    )]
    TurnRunning { conversation_id: i64 },
    #[error(
        "answer '{requested}' is no longer the latest in conversation {conversation_id}; the latest is '{latest}'"
    )]
    Stale {
        conversation_id: i64,
        requested: String,
        latest: String,
    },
    #[error("next steps are already being suggested for conversation {conversation_id}")]
    InProgress { conversation_id: i64 },
    #[error("'{value}' is not an answer of conversation {conversation_id}: {reason}")]
    InvalidAnswer {
        conversation_id: i64,
        value: String,
        reason: String,
    },
}

/// Whether a chat can have suggestions at all. The assistant chat runs on the
/// AI gateway, where one small request is cheap and attributable; harness chats
/// drive their own CLI sessions.
pub fn supports_suggestions(conversation: &ai_conversations::Model) -> bool {
    conversation.context_type == ASSISTANT_CONTEXT_TYPE
}

/// The chat's own mode, if it sets one. An unreadable stored value is treated
/// as unset so the chat falls back to the user's default.
pub fn chat_mode(conversation: &ai_conversations::Model) -> Option<SuggestionMode> {
    let stored = conversation.next_step_suggestions.as_deref()?;
    match stored.parse() {
        Ok(mode) => Some(mode),
        Err(_) => {
            tracing::warn!(
                conversation_id = conversation.id,
                stored,
                "ignoring an unknown next-step suggestion mode; using the user's default"
            );
            None
        }
    }
}

/// The mode that applies, and where it came from.
pub fn effective_mode(
    chat: Option<SuggestionMode>,
    default: SuggestionMode,
) -> (SuggestionMode, SuggestionModeSource) {
    match chat {
        Some(mode) => (mode, SuggestionModeSource::Chat),
        None => (default, SuggestionModeSource::Default),
    }
}

/// The user's default, or [`SuggestionMode::DEFAULT`] when they never set one.
pub async fn user_default(
    db: &DatabaseConnection,
    user_id: i32,
) -> Result<SuggestionMode, SuggestionError> {
    let row = ai_user_preferences::Entity::find_by_id(user_id)
        .one(db)
        .await
        .map_err(|source| SuggestionError::Preferences {
            operation: "read the assistant preferences",
            user_id,
            source,
        })?;
    Ok(row
        .and_then(|row| row.next_step_suggestions.parse().ok())
        .unwrap_or(SuggestionMode::DEFAULT))
}

/// Set the user's default for every chat that does not set its own.
pub async fn set_user_default(
    db: &DatabaseConnection,
    user_id: i32,
    mode: SuggestionMode,
) -> Result<SuggestionMode, SuggestionError> {
    let now = Utc::now();
    ai_user_preferences::Entity::insert(ai_user_preferences::ActiveModel {
        user_id: Set(user_id),
        next_step_suggestions: Set(mode.as_str().to_string()),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict(
        OnConflict::column(ai_user_preferences::Column::UserId)
            .update_columns([
                ai_user_preferences::Column::NextStepSuggestions,
                ai_user_preferences::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec(db)
    .await
    .map_err(|source| SuggestionError::Preferences {
        operation: "save the assistant preferences",
        user_id,
        source,
    })?;
    Ok(mode)
}

/// Set (or, with `None`, clear) the chat's own mode.
pub async fn set_chat_mode(
    db: &DatabaseConnection,
    conversation: &ai_conversations::Model,
    mode: Option<SuggestionMode>,
) -> Result<ai_conversations::Model, SuggestionError> {
    ai_conversations::ActiveModel {
        id: Set(conversation.id),
        next_step_suggestions: Set(mode.map(|mode| mode.as_str().to_string())),
        ..Default::default()
    }
    .update(db)
    .await
    .map_err(|source| SuggestionError::Conversation {
        operation: "save the next-step suggestion mode",
        conversation_id: conversation.id,
        source,
    })
}

/// Suggestions for one answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct NextSteps {
    /// Cursor of the answer they follow on from, as in the message list.
    pub answer: String,
    pub suggestions: Vec<String>,
    /// The model that wrote them.
    pub model: String,
    /// True when they were saved earlier and returned without a new request.
    pub saved: bool,
}

/// What is stored on an answer once suggestions were made for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedNextSteps {
    pub suggestions: Vec<String>,
    pub model: String,
}

/// The suggestions saved on a message, if any.
pub fn saved_next_steps(metadata: Option<&serde_json::Value>) -> Option<SavedNextSteps> {
    let saved = metadata?.get(SAVED_KEY)?;
    serde_json::from_value::<SavedNextSteps>(saved.clone())
        .ok()
        .filter(|saved| !saved.suggestions.is_empty())
}

/// The latest answer and the question before it.
struct Exchange {
    question: String,
    answer: String,
    answer_id: i64,
    /// The turn stopped before the answer was finished.
    cut_off: bool,
    saved: Option<SavedNextSteps>,
}

fn is_draft(row: &ai_messages::Model) -> bool {
    row.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("draft"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Picks the exchange from recent rows, newest first.
///
/// An answer still marked as a draft counts: suggestions are only made while
/// no turn runs, so a draft then belongs to a turn that was interrupted (the
/// server restarted under it) and nothing will write to it again. It is the
/// answer the user is looking at, and skipping it would suggest next steps
/// for an older one.
fn latest_exchange(newest_first: &[ai_messages::Model]) -> Option<Exchange> {
    let answer_index = newest_first
        .iter()
        .position(|row| row.role == "assistant" && !row.content.trim().is_empty())?;
    let answer = &newest_first[answer_index];
    let question = newest_first[answer_index + 1..]
        .iter()
        .find(|row| row.role == "user")?;
    Some(Exchange {
        question: clip(question.content.trim(), MAX_QUESTION_BYTES).to_string(),
        answer: clip(answer.content.trim(), MAX_ANSWER_BYTES).to_string(),
        answer_id: answer.id,
        cut_off: is_draft(answer),
        saved: saved_next_steps(answer.metadata.as_ref()),
    })
}

fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The model's lines as suggestions: list markers and quotes removed, blank
/// and repeated lines dropped, long ones cut at a word, at most
/// [`MAX_SUGGESTIONS`].
pub fn parse_suggestions(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::with_capacity(MAX_SUGGESTIONS);
    for line in text.lines() {
        let trimmed = line
            .trim()
            .trim_start_matches(|c: char| {
                c.is_ascii_digit() || matches!(c, '-' | '*' | '•' | '.' | ')' | ' ')
            })
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”'))
            .trim();
        if trimmed.is_empty() {
            continue;
        }
        let suggestion = shorten(trimmed);
        if found
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&suggestion))
        {
            continue;
        }
        found.push(suggestion);
        if found.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    found
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= MAX_SUGGESTION_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX_SUGGESTION_CHARS).collect();
    match cut.rfind(' ') {
        Some(space) if space > MAX_SUGGESTION_CHARS / 2 => format!("{}…", &cut[..space]),
        _ => format!("{cut}…"),
    }
}

/// Chats with a suggestion request running, so two tabs (or a double click)
/// do not both pay for the same answer.
static IN_FLIGHT: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

struct InFlight(i64);

impl InFlight {
    fn claim(conversation_id: i64) -> Result<Self, SuggestionError> {
        let mut set = IN_FLIGHT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if set.insert(conversation_id) {
            Ok(Self(conversation_id))
        } else {
            Err(SuggestionError::InProgress { conversation_id })
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut set = IN_FLIGHT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set.remove(&self.0);
    }
}

/// Save suggestions on the answer, unless a set is already there.
async fn save(
    db: &DatabaseConnection,
    message_id: i64,
    saved: &SavedNextSteps,
) -> Result<bool, DbErr> {
    let value = serde_json::to_value(saved).map_err(|error| DbErr::Json(error.to_string()))?;
    let result = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE ai_messages \
             SET metadata = COALESCE(metadata, '{}'::jsonb) || jsonb_build_object($1::text, $2::jsonb) \
             WHERE id = $3 AND (metadata IS NULL OR metadata -> $1::text IS NULL)",
            [SAVED_KEY.into(), value.into(), message_id.into()],
        ))
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Next steps after the chat's latest answer: the saved set if there is one,
/// otherwise one small request to the chat's model, saved on the answer.
///
/// `mode` is the chat's effective mode; `answer`, when given, is the answer the
/// user is looking at, and must still be the latest.
pub async fn suggest(
    ai: &Arc<dyn AiService>,
    db: &DatabaseConnection,
    conversation: &ai_conversations::Model,
    principal_id: i32,
    mode: SuggestionMode,
    answer: Option<i64>,
) -> Result<NextSteps, SuggestionError> {
    if !supports_suggestions(conversation) {
        return Err(SuggestionError::Unavailable {
            conversation_id: conversation.id,
            context_type: conversation.context_type.clone(),
        });
    }
    if mode == SuggestionMode::Off {
        return Err(SuggestionError::Off {
            conversation_id: conversation.id,
        });
    }
    if conversation.turn_status == "running" {
        return Err(SuggestionError::TurnRunning {
            conversation_id: conversation.id,
        });
    }
    let _in_flight = InFlight::claim(conversation.id)?;
    let rows = ai_messages::Entity::find()
        .filter(ai_messages::Column::ConversationId.eq(conversation.id))
        .filter(ai_messages::Column::Role.is_in(["user", "assistant"]))
        .order_by_desc(ai_messages::Column::Id)
        .limit(RECENT_ROWS)
        .all(db)
        .await
        .map_err(|source| SuggestionError::Conversation {
            operation: "read the latest messages",
            conversation_id: conversation.id,
            source,
        })?;
    let exchange = latest_exchange(&rows).ok_or(SuggestionError::NoAnswer {
        conversation_id: conversation.id,
    })?;
    let answer_cursor = encode_message_before_cursor(exchange.answer_id);
    if let Some(requested) = answer.filter(|requested| *requested != exchange.answer_id) {
        return Err(SuggestionError::Stale {
            conversation_id: conversation.id,
            requested: encode_message_before_cursor(requested),
            latest: answer_cursor,
        });
    }
    if let Some(saved) = exchange.saved {
        return Ok(NextSteps {
            answer: answer_cursor,
            suggestions: saved.suggestions,
            model: saved.model,
            saved: true,
        });
    }

    let model = conversation.ai_model.clone();
    let request = ChatTurnRequest {
        conversation_id: Some(conversation.id.to_string()),
        purpose: "chat.assistant.suggestions".to_string(),
        project_id: conversation.project_id,
        principal_id: Some(principal_id),
        provider: Some(conversation.ai_provider.clone()),
        model: Some(model.clone()),
        messages: vec![
            ChatMessage::system(SUGGESTIONS_SYSTEM_PROMPT),
            ChatMessage::user(format!(
                "User:\n{}\n\nAssistant:\n{}{}",
                exchange.question,
                exchange.answer,
                if exchange.cut_off {
                    "\n\n(The answer was cut off before it finished.)"
                } else {
                    ""
                }
            )),
        ],
        max_tokens: Some(MAX_OUTPUT_TOKENS),
        temperature: Some(0.4),
        ..Default::default()
    };
    let model_error = |reason: String| SuggestionError::Model {
        conversation_id: conversation.id,
        model: model.clone(),
        reason,
    };
    let collect = async {
        let mut stream = ai
            .chat_stream_turn(request)
            .await
            .map_err(|error| model_error(error.to_string()))?;
        let mut text = String::new();
        while let Some(delta) = stream.next().await {
            if let ChatStreamDelta::Text(chunk) =
                delta.map_err(|error| model_error(error.to_string()))?
            {
                text.push_str(&chunk);
            }
        }
        Ok::<_, SuggestionError>(text)
    };
    let text = tokio::time::timeout(MODEL_TIMEOUT, collect)
        .await
        .map_err(|_| model_error(format!("no reply within {}s", MODEL_TIMEOUT.as_secs())))??;

    let suggestions = parse_suggestions(&text);
    if suggestions.is_empty() {
        return Err(SuggestionError::Empty {
            conversation_id: conversation.id,
            model,
        });
    }
    let saved = SavedNextSteps { suggestions, model };
    // The request is paid for by now, so a failure to save is logged and the
    // suggestions are still returned; the cost is one more request next time.
    match save(db, exchange.answer_id, &saved).await {
        Ok(true) => {}
        Ok(false) => tracing::info!(
            conversation_id = conversation.id,
            message_id = exchange.answer_id,
            "next steps were already saved on this answer; keeping the earlier set"
        ),
        Err(error) => tracing::warn!(
            conversation_id = conversation.id,
            message_id = exchange.answer_id,
            %error,
            "could not save next steps on the answer"
        ),
    }
    tracing::info!(
        conversation_id = conversation.id,
        message_id = exchange.answer_id,
        count = saved.suggestions.len(),
        model = %saved.model,
        "suggested next steps"
    );
    Ok(NextSteps {
        answer: answer_cursor,
        suggestions: saved.suggestions,
        model: saved.model,
        saved: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_ai::{AiError, AiRequest, AiResponse, TokenStream};

    /// Records the request it is sent and streams a scripted reply.
    struct ScriptedModel {
        reply: Vec<&'static str>,
        seen: Mutex<Option<ChatTurnRequest>>,
    }

    impl ScriptedModel {
        fn new(reply: Vec<&'static str>) -> Arc<Self> {
            Arc::new(Self {
                reply,
                seen: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl AiService for ScriptedModel {
        async fn is_available(&self) -> bool {
            true
        }

        async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
            Err(AiError::NotAvailable)
        }

        async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
            Err(AiError::NotAvailable)
        }

        async fn chat_stream_turn(
            &self,
            request: ChatTurnRequest,
        ) -> Result<temps_ai::ChatTurnStream, AiError> {
            if let Ok(mut seen) = self.seen.lock() {
                *seen = Some(request);
            }
            let chunks: Vec<Result<ChatStreamDelta, AiError>> = self
                .reply
                .iter()
                .map(|chunk| Ok(ChatStreamDelta::Text(chunk.to_string())))
                .collect();
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    /// Each test uses its own conversation id: the in-flight guard is shared by
    /// the whole process, and tests run in parallel.
    fn conversation(id: i64, context_type: &str) -> ai_conversations::Model {
        ai_conversations::Model {
            id,
            public_id: format!("conv_test_{id}"),
            project_id: Some(5),
            application_id: None,
            context_type: context_type.to_string(),
            context_id: "assistant_1".to_string(),
            title: None,
            status: "active".to_string(),
            kind: "ai".to_string(),
            created_by: 1,
            metadata: None,
            created_at: Utc::now(),
            last_activity_at: Utc::now(),
            ai_provider: "gateway".to_string(),
            ai_model: "vendor/chat-model".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "ask".to_string(),
            cli_session_id: None,
            cli_session_fingerprint: None,
            turn_status: "idle".to_string(),
            active_turn_id: None,
            last_turn_id: None,
            turn_started_at: None,
            next_step_suggestions: None,
        }
    }

    fn assistant(id: i64) -> ai_conversations::Model {
        conversation(id, ASSISTANT_CONTEXT_TYPE)
    }

    fn exec_ok(rows_affected: u64) -> sea_orm::MockExecResult {
        sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    #[tokio::test]
    async fn suggest_sends_only_the_latest_exchange_and_saves_the_result() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                row(4, "assistant", "Your app is deployed and healthy."),
                row(3, "user", "deploy my app"),
                row(2, "assistant", "an older answer"),
                row(1, "user", "an older question"),
            ]])
            .append_exec_results([exec_ok(1)])
            .into_connection();
        let model = ScriptedModel::new(vec![
            "Show the deploy",
            "ment logs\nAdd a custom domain\n",
            "Set up backups",
        ]);
        let ai: Arc<dyn AiService> = model.clone();

        let steps = suggest(&ai, &db, &assistant(41), 9, SuggestionMode::Ask, Some(4))
            .await
            .expect("suggestions");

        assert_eq!(steps.answer, encode_message_before_cursor(4));
        assert!(!steps.saved);
        assert_eq!(steps.model, "vendor/chat-model");
        assert_eq!(
            steps.suggestions,
            vec![
                "Show the deployment logs",
                "Add a custom domain",
                "Set up backups"
            ]
        );
        let seen = model.seen.lock().unwrap().clone().expect("a request");
        assert_eq!(seen.conversation_id.as_deref(), Some("41"));
        assert_eq!(seen.principal_id, Some(9));
        assert_eq!(seen.purpose, "chat.assistant.suggestions");
        assert!(seen.tools.is_empty());
        assert_eq!(seen.messages.len(), 2);
        let prompt = &seen.messages[1].content;
        assert!(prompt.contains("deploy my app"));
        assert!(prompt.contains("Your app is deployed and healthy."));
        assert!(!prompt.contains("older"));

        // Saved on the answer, only if nothing was saved there first.
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("UPDATE ai_messages"));
        assert!(log.contains("metadata -> $1::text IS NULL"));
        assert!(log.contains("Show the deployment logs"));
    }

    #[tokio::test]
    async fn suggest_follows_an_interrupted_answer_and_says_it_was_cut_off() {
        let mut draft = row(6, "assistant", "I'll read the app source, then build");
        draft.metadata = Some(serde_json::json!({ "draft": true, "turn_id": "t1" }));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                draft,
                row(5, "user", "build a dashboard"),
                row(4, "assistant", "an older, finished answer"),
            ]])
            .append_exec_results([exec_ok(1)])
            .into_connection();
        let model = ScriptedModel::new(vec!["Carry on building the dashboard"]);
        let ai: Arc<dyn AiService> = model.clone();

        let steps = suggest(&ai, &db, &assistant(51), 9, SuggestionMode::Ask, Some(6))
            .await
            .expect("suggestions for the interrupted answer");

        assert_eq!(steps.answer, encode_message_before_cursor(6));
        let seen = model.seen.lock().unwrap().clone().expect("a request");
        let prompt = &seen.messages[1].content;
        assert!(prompt.contains("I'll read the app source, then build"));
        assert!(prompt.contains("cut off before it finished"));
        assert!(!prompt.contains("older"));
    }

    #[tokio::test]
    async fn saved_suggestions_are_returned_without_asking_the_model_again() {
        let mut answer = row(4, "assistant", "Deployed.");
        answer.metadata = Some(serde_json::json!({
            "parts": [],
            "next_steps": { "suggestions": ["Show the logs"], "model": "vendor/earlier" }
        }));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![answer, row(3, "user", "deploy")]])
            .into_connection();
        let model = ScriptedModel::new(vec!["Something new"]);
        let ai: Arc<dyn AiService> = model.clone();

        let steps = suggest(&ai, &db, &assistant(42), 9, SuggestionMode::Always, None)
            .await
            .expect("saved suggestions");

        assert!(steps.saved);
        assert_eq!(steps.suggestions, vec!["Show the logs"]);
        assert_eq!(steps.model, "vendor/earlier");
        assert!(model.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_failure_to_save_still_returns_what_was_paid_for() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(2, "assistant", "done"), row(1, "user", "go")]])
            .append_exec_errors([DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let ai: Arc<dyn AiService> = ScriptedModel::new(vec!["Check the logs"]);

        let steps = suggest(&ai, &db, &assistant(43), 9, SuggestionMode::Ask, None)
            .await
            .expect("suggestions");

        assert_eq!(steps.suggestions, vec!["Check the logs"]);
    }

    #[tokio::test]
    async fn suggest_refuses_chats_other_than_the_assistant() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let model = ScriptedModel::new(vec!["anything"]);
        let ai: Arc<dyn AiService> = model.clone();

        let error = suggest(
            &ai,
            &db,
            &conversation(44, "deployment"),
            9,
            SuggestionMode::Always,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            SuggestionError::Unavailable {
                conversation_id: 44,
                ..
            }
        ));
        assert!(model.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn suggest_is_refused_when_suggestions_are_off() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let model = ScriptedModel::new(vec!["anything"]);
        let ai: Arc<dyn AiService> = model.clone();

        let error = suggest(&ai, &db, &assistant(45), 9, SuggestionMode::Off, None)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            SuggestionError::Off {
                conversation_id: 45
            }
        ));
        assert!(model.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn suggest_waits_for_the_turn_to_end() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let model = ScriptedModel::new(vec!["anything"]);
        let ai: Arc<dyn AiService> = model.clone();
        let mut running = assistant(46);
        running.turn_status = "running".to_string();

        let error = suggest(&ai, &db, &running, 9, SuggestionMode::Always, None)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            SuggestionError::TurnRunning {
                conversation_id: 46
            }
        ));
        assert!(model.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn suggest_refuses_an_answer_that_is_no_longer_the_latest() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                row(6, "assistant", "a newer answer"),
                row(5, "user", "and now?"),
                row(4, "assistant", "the answer on screen"),
            ]])
            .into_connection();
        let model = ScriptedModel::new(vec!["anything"]);
        let ai: Arc<dyn AiService> = model.clone();

        let error = suggest(&ai, &db, &assistant(47), 9, SuggestionMode::Ask, Some(4))
            .await
            .unwrap_err();

        match &error {
            SuggestionError::Stale {
                requested, latest, ..
            } => {
                assert_eq!(requested, &encode_message_before_cursor(4));
                assert_eq!(latest, &encode_message_before_cursor(6));
            }
            other => panic!("expected Stale, got {other:?}"),
        }
        assert!(model.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn only_one_suggestion_request_runs_per_chat() {
        let first = InFlight::claim(48).expect("first claim");
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let model = ScriptedModel::new(vec!["anything"]);
        let ai: Arc<dyn AiService> = model.clone();

        let error = suggest(&ai, &db, &assistant(48), 9, SuggestionMode::Ask, None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            SuggestionError::InProgress {
                conversation_id: 48
            }
        ));
        assert!(model.seen.lock().unwrap().is_none());

        drop(first);
        assert!(
            InFlight::claim(48).is_ok(),
            "released when the request ends"
        );
    }

    #[tokio::test]
    async fn suggest_without_an_answer_spends_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(1, "user", "hello")]])
            .into_connection();
        let model = ScriptedModel::new(vec!["anything"]);
        let ai: Arc<dyn AiService> = model.clone();

        let error = suggest(&ai, &db, &assistant(49), 9, SuggestionMode::Ask, None)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            SuggestionError::NoAnswer {
                conversation_id: 49
            }
        ));
        assert!(model.seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn an_empty_reply_is_an_error_naming_the_model() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(2, "assistant", "done"), row(1, "user", "go")]])
            .into_connection();
        let ai: Arc<dyn AiService> = ScriptedModel::new(vec!["\n  \n"]);

        let error = suggest(&ai, &db, &assistant(50), 9, SuggestionMode::Ask, None)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("vendor/chat-model"));
        assert!(matches!(error, SuggestionError::Empty { .. }));
    }

    #[test]
    fn saved_suggestions_are_read_from_message_metadata() {
        let saved = saved_next_steps(Some(&serde_json::json!({
            "next_steps": { "suggestions": ["a", "b"], "model": "m" }
        })))
        .expect("saved");
        assert_eq!(saved.suggestions, vec!["a", "b"]);
        assert!(saved_next_steps(None).is_none());
        assert!(saved_next_steps(Some(&serde_json::json!({ "tools": [] }))).is_none());
        assert!(saved_next_steps(Some(&serde_json::json!({
            "next_steps": { "suggestions": [], "model": "m" }
        })))
        .is_none());
    }

    fn row(id: i64, role: &str, content: &str) -> ai_messages::Model {
        ai_messages::Model {
            id,
            conversation_id: 1,
            role: role.to_string(),
            content: content.to_string(),
            metadata: None,
            tokens_in: None,
            tokens_out: None,
            cost_microcents: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn modes_round_trip_and_reject_unknown_values() {
        for mode in [
            SuggestionMode::Off,
            SuggestionMode::Ask,
            SuggestionMode::Always,
        ] {
            assert_eq!(mode.as_str().parse::<SuggestionMode>().unwrap(), mode);
        }
        let error = "sometimes".parse::<SuggestionMode>().unwrap_err();
        assert!(error.to_string().contains("'sometimes'"));
    }

    #[test]
    fn a_chat_setting_overrides_the_default() {
        assert_eq!(
            effective_mode(Some(SuggestionMode::Off), SuggestionMode::Always),
            (SuggestionMode::Off, SuggestionModeSource::Chat)
        );
        assert_eq!(
            effective_mode(None, SuggestionMode::Always),
            (SuggestionMode::Always, SuggestionModeSource::Default)
        );
    }

    #[test]
    fn the_exchange_is_the_newest_answer_and_its_question() {
        let newest_first = vec![
            row(3, "assistant", "The deployment is healthy."),
            row(2, "user", "is my app up?"),
            row(1, "assistant", "Hello."),
        ];
        let exchange = latest_exchange(&newest_first).expect("an exchange");
        assert_eq!(exchange.answer_id, 3);
        assert_eq!(exchange.question, "is my app up?");
        assert_eq!(exchange.answer, "The deployment is healthy.");
        assert!(!exchange.cut_off);
    }

    #[test]
    fn an_interrupted_answer_is_the_latest_and_marked_cut_off() {
        let mut draft = row(5, "assistant", "I'll check the logs, then");
        draft.metadata = Some(serde_json::json!({ "draft": true }));
        let newest_first = vec![
            draft,
            row(4, "user", "and the logs?"),
            row(3, "assistant", "The deployment is healthy."),
            row(2, "user", "is my app up?"),
        ];
        let exchange = latest_exchange(&newest_first).expect("an exchange");
        assert_eq!(exchange.answer_id, 5);
        assert_eq!(exchange.question, "and the logs?");
        assert!(exchange.cut_off);
    }

    #[test]
    fn an_answer_with_no_text_yet_is_passed_over() {
        let mut tools_only = row(5, "assistant", "");
        tools_only.metadata = Some(serde_json::json!({ "draft": true }));
        let newest_first = vec![
            tools_only,
            row(4, "user", "and the logs?"),
            row(3, "assistant", "The deployment is healthy."),
            row(2, "user", "is my app up?"),
        ];
        assert_eq!(
            latest_exchange(&newest_first)
                .expect("an exchange")
                .answer_id,
            3
        );
    }

    #[test]
    fn no_exchange_without_an_answer_or_its_question() {
        assert!(latest_exchange(&[row(1, "user", "hi")]).is_none());
        assert!(latest_exchange(&[row(1, "assistant", "welcome")]).is_none());
        assert!(latest_exchange(&[]).is_none());
    }

    #[test]
    fn long_messages_are_clipped_before_they_are_sent() {
        let long = "é".repeat(MAX_ANSWER_BYTES);
        let newest_first = vec![row(2, "assistant", &long), row(1, "user", "q")];
        let exchange = latest_exchange(&newest_first).expect("an exchange");
        assert!(exchange.answer.len() <= MAX_ANSWER_BYTES);
    }

    #[test]
    fn model_lines_become_clean_distinct_suggestions() {
        let text = "1. Show the deployment logs\n- \"Roll back to the previous deployment\"\n\n* show the deployment logs\n• Add a custom domain\nExtra line";
        assert_eq!(
            parse_suggestions(text),
            vec![
                "Show the deployment logs",
                "Roll back to the previous deployment",
                "Add a custom domain",
            ]
        );
        assert!(parse_suggestions("  \n\n").is_empty());
    }

    #[test]
    fn a_long_suggestion_is_cut_at_a_word() {
        let long = format!("Check {}", "the logs ".repeat(30));
        let parsed = parse_suggestions(&long);
        assert!(parsed[0].ends_with('…'));
        assert!(parsed[0].chars().count() <= MAX_SUGGESTION_CHARS + 1);
    }

    #[tokio::test]
    async fn a_user_without_preferences_gets_ask() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_user_preferences::Model>::new()])
            .into_connection();
        assert_eq!(user_default(&db, 7).await.unwrap(), SuggestionMode::Ask);
    }

    #[tokio::test]
    async fn a_saved_default_is_read_back() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![ai_user_preferences::Model {
                user_id: 7,
                next_step_suggestions: "always".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }]])
            .into_connection();
        assert_eq!(user_default(&db, 7).await.unwrap(), SuggestionMode::Always);
    }

    #[tokio::test]
    async fn a_database_failure_names_the_user() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let error = user_default(&db, 7).await.unwrap_err();
        assert!(matches!(
            error,
            SuggestionError::Preferences { user_id: 7, .. }
        ));
        assert!(error.to_string().contains("user 7"));
    }
}
