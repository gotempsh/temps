// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Next-step suggestions: the user's default, a chat's own mode, and asking
//! for suggestions after an answer (see [`crate::suggestions`]).
//!
//! These are the user's own controls, so an assistant tool call is refused on
//! every one of them: the model must not switch suggestions on for itself or
//! spend tokens asking for them.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::{
    ai_tool_call::{require_human_caller, AiToolCall},
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, RequestMetadata,
};
use utoipa::{OpenApi, ToSchema};

use crate::audit::{
    AiNextStepsSuggestedAudit, AiSuggestionChatModeUpdatedAudit, AiSuggestionDefaultUpdatedAudit,
};
use crate::handlers::{ensure_user_conversation_access, AppState};
use crate::service::decode_message_before_cursor;
use crate::suggestions::{
    chat_mode, effective_mode, supports_suggestions, NextSteps, SuggestionError, SuggestionMode,
    SuggestionModeSource,
};

impl From<SuggestionError> for Problem {
    fn from(error: SuggestionError) -> Self {
        match error {
            SuggestionError::InvalidMode { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Suggestion Mode")
                .with_detail(error.to_string()),
            SuggestionError::Unavailable { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Suggestions Not Available")
                .with_detail(error.to_string()),
            SuggestionError::NoAnswer { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("No Answer Yet")
                .with_detail(error.to_string()),
            SuggestionError::Off { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Suggestions Are Off")
                .with_detail(error.to_string()),
            SuggestionError::TurnRunning { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Still Answering")
                .with_detail(error.to_string()),
            SuggestionError::Stale { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Answer No Longer Latest")
                .with_detail(error.to_string()),
            SuggestionError::InProgress { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Already Suggesting")
                .with_detail(error.to_string()),
            SuggestionError::InvalidAnswer { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Answer")
                .with_detail(error.to_string()),
            SuggestionError::Model { .. } | SuggestionError::Empty { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Suggestion Model Failed")
                    .with_detail(error.to_string())
            }
            SuggestionError::Preferences { .. } | SuggestionError::Conversation { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// The user's default for chats that do not set their own mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SuggestionDefault {
    pub mode: SuggestionMode,
}

/// A chat's next-step suggestion setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChatSuggestionSettings {
    /// Whether this chat can have suggestions at all. Only the assistant chat
    /// can; the other fields still describe the setting for other chats.
    pub available: bool,
    /// What happens after each answer in this chat.
    pub mode: SuggestionMode,
    /// Whether `mode` is the chat's own setting or the user's default.
    pub source: SuggestionModeSource,
    /// The chat's own setting, when it has one.
    pub chat_mode: Option<SuggestionMode>,
    /// The user's default, which applies when `chat_mode` is unset.
    pub default_mode: SuggestionMode,
}

impl ChatSuggestionSettings {
    fn new(available: bool, chat: Option<SuggestionMode>, default: SuggestionMode) -> Self {
        let (mode, source) = effective_mode(chat, default);
        Self {
            available,
            mode,
            source,
            chat_mode: chat,
            default_mode: default,
        }
    }
}

/// Which answer to suggest next steps for.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct SuggestNextStepsRequest {
    /// Cursor of the answer the user is looking at, from the message list. When
    /// given, it must still be the chat's latest answer. Without it the latest
    /// answer is used.
    #[serde(default)]
    pub answer: Option<String>,
}

/// Set a chat's own mode, or `null` to follow the user's default.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateChatSuggestionModeRequest {
    #[serde(default)]
    pub mode: Option<SuggestionMode>,
}

fn audit_context(
    auth: &temps_auth::context::AuthContext,
    metadata: &RequestMetadata,
) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// The user's default for next-step suggestions.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/preferences/suggestions",
    responses(
        (status = 200, description = "The user's default", body = SuggestionDefault),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_suggestion_default(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    ai_call: Option<Extension<AiToolCall>>,
) -> Result<Json<SuggestionDefault>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    require_human_caller(ai_call.as_ref().map(|e| &e.0), "get_suggestion_default")?;
    let mode = state.service.suggestion_default(auth.user_id()).await?;
    Ok(Json(SuggestionDefault { mode }))
}

/// Set the user's default for every chat that does not set its own mode.
#[utoipa::path(
    put, tag = "AI Chat", path = "/ai/preferences/suggestions",
    request_body = SuggestionDefault,
    responses(
        (status = 200, description = "The saved default", body = SuggestionDefault),
        (status = 400, description = "Invalid mode", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_suggestion_default(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<AiToolCall>>,
    Json(request): Json<SuggestionDefault>,
) -> Result<Json<SuggestionDefault>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    require_human_caller(ai_call.as_ref().map(|e| &e.0), "update_suggestion_default")?;
    let mode = state
        .service
        .set_suggestion_default(auth.user_id(), request.mode)
        .await?;
    state
        .audit(&AiSuggestionDefaultUpdatedAudit {
            context: audit_context(&auth, &metadata),
            mode: mode.as_str().to_string(),
        })
        .await;
    Ok(Json(SuggestionDefault { mode }))
}

/// A chat's next-step suggestion setting, with the default it falls back to.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/suggestion-mode",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "The chat's setting", body = ChatSuggestionSettings),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_chat_suggestion_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    ai_call: Option<Extension<AiToolCall>>,
    Path(public_id): Path<String>,
) -> Result<Json<ChatSuggestionSettings>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    require_human_caller(ai_call.as_ref().map(|e| &e.0), "get_chat_suggestion_mode")?;
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let default = state.service.suggestion_default(auth.user_id()).await?;
    Ok(Json(ChatSuggestionSettings::new(
        supports_suggestions(&conversation),
        chat_mode(&conversation),
        default,
    )))
}

/// Set a chat's own next-step suggestion mode, or clear it to follow the
/// user's default.
#[utoipa::path(
    put, tag = "AI Chat", path = "/ai/conversations/{public_id}/suggestion-mode",
    params(("public_id" = String, Path, description = "Conversation public id")),
    request_body = UpdateChatSuggestionModeRequest,
    responses(
        (status = 200, description = "The chat's new setting", body = ChatSuggestionSettings),
        (status = 400, description = "Invalid mode", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 409, description = "This chat cannot have suggestions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_chat_suggestion_mode(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<AiToolCall>>,
    Path(public_id): Path<String>,
    Json(request): Json<UpdateChatSuggestionModeRequest>,
) -> Result<Json<ChatSuggestionSettings>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    require_human_caller(
        ai_call.as_ref().map(|e| &e.0),
        "update_chat_suggestion_mode",
    )?;
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    if !supports_suggestions(&conversation) {
        return Err(SuggestionError::Unavailable {
            conversation_id: conversation.id,
            context_type: conversation.context_type.clone(),
        }
        .into());
    }
    let updated = state
        .service
        .set_chat_suggestion_mode(&conversation, request.mode)
        .await?;
    state
        .audit(&AiSuggestionChatModeUpdatedAudit {
            context: audit_context(&auth, &metadata),
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            mode: request.mode.map(|mode| mode.as_str().to_string()),
        })
        .await;
    let default = state.service.suggestion_default(auth.user_id()).await?;
    Ok(Json(ChatSuggestionSettings::new(
        true,
        chat_mode(&updated),
        default,
    )))
}

/// Suggest a few next steps after the chat's latest answer. The first request
/// for an answer makes one small request to the chat's model, with only the
/// last question and answer, and its cost counts toward the chat's spend. The
/// result is saved on the answer, and later requests for it return the saved
/// set at no cost.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/suggestions",
    params(("public_id" = String, Path, description = "Conversation public id")),
    request_body = SuggestNextStepsRequest,
    responses(
        (status = 200, description = "Suggested next steps", body = NextSteps),
        (status = 400, description = "The answer cursor is malformed", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 409, description = "Suggestions are off or unavailable in this chat, a turn is running, the answer is no longer the latest, there is no answer yet, or a suggestion is already being made", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 502, description = "The model failed to suggest anything", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn suggest_next_steps(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<AiToolCall>>,
    Path(public_id): Path<String>,
    Json(request): Json<SuggestNextStepsRequest>,
) -> Result<Json<NextSteps>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    require_human_caller(ai_call.as_ref().map(|e| &e.0), "suggest_next_steps")?;
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let answer = request
        .answer
        .as_deref()
        .map(|cursor| {
            decode_message_before_cursor(cursor).map_err(|error| SuggestionError::InvalidAnswer {
                conversation_id: conversation.id,
                value: cursor.to_string(),
                reason: error.to_string(),
            })
        })
        .transpose()?;
    let steps = state
        .service
        .suggest_next_steps(&conversation, auth.user_id(), answer)
        .await?;
    // Only a new set spent model quota and was written to the answer; the
    // audit logger reports its own failure without failing the request.
    if let Some(entry) = next_steps_audit(
        audit_context(&auth, &metadata),
        &conversation,
        answer,
        &steps,
    ) {
        state.audit(&entry).await;
    }
    Ok(Json(steps))
}

/// The audit entry for a suggestion request, or `None` when the saved set was
/// returned and nothing was spent or written.
fn next_steps_audit(
    context: AuditContext,
    conversation: &temps_entities::ai_conversations::Model,
    requested_answer: Option<i64>,
    steps: &NextSteps,
) -> Option<AiNextStepsSuggestedAudit> {
    if steps.saved {
        return None;
    }
    Some(AiNextStepsSuggestedAudit {
        context,
        project_id: conversation.project_id,
        conversation_id: conversation.public_id.clone(),
        message_id: decode_message_before_cursor(&steps.answer)
            .ok()
            .or(requested_answer),
        suggestion_count: steps.suggestions.len(),
        model: steps.model.clone(),
    })
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/ai/preferences/suggestions",
            get(get_suggestion_default).put(update_suggestion_default),
        )
        .route(
            "/ai/conversations/{public_id}/suggestion-mode",
            get(get_chat_suggestion_mode).put(update_chat_suggestion_mode),
        )
        .route(
            "/ai/conversations/{public_id}/suggestions",
            post(suggest_next_steps),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_suggestion_default,
        update_suggestion_default,
        get_chat_suggestion_mode,
        update_chat_suggestion_mode,
        suggest_next_steps
    ),
    components(schemas(
        SuggestionMode,
        SuggestionModeSource,
        SuggestionDefault,
        ChatSuggestionSettings,
        UpdateChatSuggestionModeRequest,
        SuggestNextStepsRequest,
        NextSteps
    ))
)]
pub struct SuggestionApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_the_status_a_client_can_act_on() {
        let status = |error: SuggestionError| Problem::from(error).status_code;
        assert_eq!(
            status(SuggestionError::InvalidMode { value: "x".into() }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(SuggestionError::Unavailable {
                conversation_id: 7,
                context_type: "deployment".into()
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(SuggestionError::NoAnswer { conversation_id: 7 }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(SuggestionError::Model {
                conversation_id: 7,
                model: "m".into(),
                reason: "down".into()
            }),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            status(SuggestionError::Empty {
                conversation_id: 7,
                model: "m".into()
            }),
            StatusCode::BAD_GATEWAY
        );
        for conflict in [
            SuggestionError::Off { conversation_id: 7 },
            SuggestionError::TurnRunning { conversation_id: 7 },
            SuggestionError::InProgress { conversation_id: 7 },
            SuggestionError::Stale {
                conversation_id: 7,
                requested: "m1_1".into(),
                latest: "m1_2".into(),
            },
        ] {
            assert_eq!(status(conflict), StatusCode::CONFLICT);
        }
        assert_eq!(
            status(SuggestionError::InvalidAnswer {
                conversation_id: 7,
                value: "x".into(),
                reason: "bad".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(SuggestionError::Preferences {
                operation: "read",
                user_id: 3,
                source: sea_orm::DbErr::Custom("x".into())
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status(SuggestionError::Conversation {
                operation: "save",
                conversation_id: 7,
                source: sea_orm::DbErr::Custom("x".into())
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn settings_say_where_the_mode_comes_from() {
        let following = ChatSuggestionSettings::new(true, None, SuggestionMode::Always);
        assert_eq!(following.mode, SuggestionMode::Always);
        assert_eq!(following.source, SuggestionModeSource::Default);
        assert_eq!(following.chat_mode, None);

        let own = ChatSuggestionSettings::new(true, Some(SuggestionMode::Off), SuggestionMode::Ask);
        assert_eq!(own.mode, SuggestionMode::Off);
        assert_eq!(own.source, SuggestionModeSource::Chat);
        assert_eq!(own.default_mode, SuggestionMode::Ask);
    }

    #[test]
    fn a_null_or_missing_mode_clears_the_chat_setting() {
        let cleared: UpdateChatSuggestionModeRequest =
            serde_json::from_str(r#"{"mode":null}"#).unwrap();
        assert_eq!(cleared.mode, None);
        let missing: UpdateChatSuggestionModeRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(missing.mode, None);
        let set: UpdateChatSuggestionModeRequest =
            serde_json::from_str(r#"{"mode":"always"}"#).unwrap();
        assert_eq!(set.mode, Some(SuggestionMode::Always));
        assert!(
            serde_json::from_str::<UpdateChatSuggestionModeRequest>(r#"{"mode":"later"}"#).is_err()
        );
    }

    fn conversation() -> temps_entities::ai_conversations::Model {
        temps_entities::ai_conversations::Model {
            id: 7,
            public_id: "conv_test_7".to_string(),
            project_id: Some(5),
            application_id: None,
            context_type: "assistant".to_string(),
            context_id: "assistant_1".to_string(),
            title: None,
            status: "active".to_string(),
            kind: "ai".to_string(),
            created_by: 1,
            metadata: None,
            created_at: chrono::Utc::now(),
            last_activity_at: chrono::Utc::now(),
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

    fn context() -> AuditContext {
        AuditContext {
            user_id: 3,
            ip_address: Some("127.0.0.1".to_string()),
            user_agent: "test".to_string(),
        }
    }

    fn steps(saved: bool) -> NextSteps {
        NextSteps {
            answer: crate::service::encode_message_before_cursor(42),
            suggestions: vec![
                "Check the deployment logs".to_string(),
                "Add a custom domain".to_string(),
            ],
            model: "vendor/small-model".to_string(),
            saved,
        }
    }

    #[test]
    fn a_new_set_of_suggestions_is_audited_by_count_not_text() {
        use temps_core::AuditOperation;
        let entry = next_steps_audit(context(), &conversation(), None, &steps(false))
            .expect("a newly generated set is audited");
        assert_eq!(entry.conversation_id, "conv_test_7");
        assert_eq!(entry.project_id, Some(5));
        assert_eq!(entry.message_id, Some(42));
        assert_eq!(entry.suggestion_count, 2);
        assert_eq!(entry.model, "vendor/small-model");
        assert_eq!(entry.operation_type(), "ai.suggestions.generated");
        let body = AuditOperation::serialize(&entry).expect("serialize audit");
        assert!(!body.contains("deployment logs"), "{body}");
    }

    #[test]
    fn returning_the_saved_set_is_not_audited() {
        assert!(next_steps_audit(context(), &conversation(), Some(42), &steps(true)).is_none());
    }

    #[test]
    fn an_unreadable_answer_cursor_falls_back_to_the_requested_answer() {
        let mut generated = steps(false);
        generated.answer = "not-a-cursor".to_string();
        let entry = next_steps_audit(context(), &conversation(), Some(41), &generated);
        assert_eq!(entry.map(|entry| entry.message_id), Some(Some(41)));
    }
}
