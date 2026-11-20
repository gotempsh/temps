// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The "Compact now" endpoint: summarize a chat's older messages on request,
//! instead of waiting for the automatic threshold (see [`crate::compaction`]).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Extension, Json, Router,
};
use serde::Serialize;
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, RequestMetadata,
};
use utoipa::{OpenApi, ToSchema};

use crate::audit::AiConversationCompactedAudit;
use crate::compaction::{CompactionError, CompactionOutcome};
use crate::handlers::{ensure_user_conversation_access, AppState};

impl From<CompactionError> for Problem {
    fn from(error: CompactionError) -> Self {
        match error {
            CompactionError::InProgress { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Already Compacting")
                .with_detail(error.to_string()),
            CompactionError::Model { .. } | CompactionError::EmptySummary { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("Summary Model Failed")
                    .with_detail(error.to_string())
            }
            CompactionError::Load { .. } | CompactionError::Store { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// What "Compact now" did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CompactConversationResponse {
    /// Whether older messages were folded into the summary by this request.
    pub compacted: bool,
    /// Messages folded into the summary by this request.
    pub folded_messages: usize,
    /// Messages the model still sees word for word after the summary.
    pub kept_messages: usize,
    /// The model that wrote the summary, when one did.
    pub model: Option<String>,
    /// Why nothing was compacted, when `compacted` is false.
    pub reason: Option<String>,
}

impl From<&CompactionOutcome> for CompactConversationResponse {
    fn from(outcome: &CompactionOutcome) -> Self {
        match outcome {
            CompactionOutcome::Compacted {
                folded,
                kept,
                model,
            } => Self {
                compacted: true,
                folded_messages: *folded,
                kept_messages: *kept,
                model: Some(model.clone()),
                reason: None,
            },
            CompactionOutcome::NothingToCompact { messages } => Self {
                compacted: false,
                folded_messages: 0,
                kept_messages: *messages,
                model: None,
                reason: Some(format!(
                    "Only {messages} recent message(s) are replayed to the model, which is too few to be worth summarizing."
                )),
            },
        }
    }
}

/// Summarize the chat's older messages now. The full transcript stays on
/// screen; only what the model is sent on later turns shrinks.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/compact",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "Compaction finished, or there was nothing to compact", body = CompactConversationResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 409, description = "The chat is already being compacted", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 502, description = "The summary model failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn compact_conversation(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
) -> Result<Json<CompactConversationResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let outcome = state.service.compact_now(&conversation).await?;
    if let CompactionOutcome::Compacted {
        folded,
        kept,
        model,
    } = &outcome
    {
        state
            .audit(&AiConversationCompactedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                project_id: conversation.project_id,
                conversation_id: conversation.public_id.clone(),
                folded_messages: *folded,
                kept_messages: *kept,
                summary_model: model.clone(),
            })
            .await;
    }
    Ok(Json(CompactConversationResponse::from(&outcome)))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/ai/conversations/{public_id}/compact",
        post(compact_conversation),
    )
}

#[derive(OpenApi)]
#[openapi(
    paths(compact_conversation),
    components(schemas(CompactConversationResponse))
)]
pub struct CompactionApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_the_status_a_client_can_act_on() {
        let status = |error: CompactionError| Problem::from(error).status_code;
        assert_eq!(
            status(CompactionError::InProgress { conversation_id: 7 }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(CompactionError::Model {
                conversation_id: 7,
                model: "m".into(),
                reason: "down".into()
            }),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            status(CompactionError::EmptySummary {
                conversation_id: 7,
                model: "m".into()
            }),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            status(CompactionError::Load {
                conversation_id: 7,
                source: sea_orm::DbErr::Custom("x".into())
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status(CompactionError::Store {
                conversation_id: 7,
                source: sea_orm::DbErr::Custom("x".into())
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn nothing_to_compact_explains_itself() {
        let response =
            CompactConversationResponse::from(&CompactionOutcome::NothingToCompact { messages: 3 });
        assert!(!response.compacted);
        assert_eq!(response.kept_messages, 3);
        assert!(response.reason.is_some_and(|r| r.contains('3')));
    }

    #[test]
    fn a_compaction_reports_what_it_folded() {
        let response = CompactConversationResponse::from(&CompactionOutcome::Compacted {
            folded: 12,
            kept: 6,
            model: "vendor/small".into(),
        });
        assert!(response.compacted);
        assert_eq!(response.folded_messages, 12);
        assert_eq!(response.model.as_deref(), Some("vendor/small"));
        assert_eq!(response.reason, None);
    }
}
