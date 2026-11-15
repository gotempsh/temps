// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The private form behind the assistant's `request_secret`: list the
//! credentials a chat is waiting for, type one in, or discard it.
//!
//! These are human-only. The assistant must never be able to fill its own slot
//! (it would defeat the point of asking the user), so each handler refuses a
//! request the assistant issued itself, on top of the tool allowlist.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Extension, Json, Router,
};
use serde::Deserialize;
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::{
    ai_tool_call::{require_human_caller, AiToolCall},
    problemdetails::{self, Problem, ProblemDetails},
    secret_string::SecretString,
    AuditContext, RequestMetadata,
};
use utoipa::{OpenApi, ToSchema};

use crate::audit::{AiSecretDiscardedAudit, AiSecretProvidedAudit};
use crate::handlers::{ensure_user_conversation_access, AppState};
use crate::secret_slots::{SecretSlotError, SecretSlotView};

impl From<SecretSlotError> for Problem {
    fn from(error: SecretSlotError) -> Self {
        match error {
            SecretSlotError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Secret Request Not Found")
                .with_detail(error.to_string()),
            SecretSlotError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Secret")
                .with_detail(error.to_string()),
            SecretSlotError::NotOpen { .. } | SecretSlotError::TooManyOpen { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Secret Request Closed")
                    .with_detail(error.to_string())
            }
            SecretSlotError::Expired { .. } => problemdetails::new(StatusCode::GONE)
                .with_title("Secret Request Expired")
                .with_detail(error.to_string()),
            SecretSlotError::Encryption { .. } | SecretSlotError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct FillSecretRequest {
    /// The credential. Stored encrypted; never returned and never shown to the
    /// assistant, which only receives an opaque handle.
    pub value: SecretString,
}

/// List the credentials this chat has asked for and not yet used up or lost.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/secrets",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "Open secret requests, never including values", body = Vec<SecretSlotView>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_secret_requests(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    ai_call: Option<Extension<AiToolCall>>,
    Path(public_id): Path<String>,
) -> Result<Json<Vec<SecretSlotView>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    require_human_caller(ai_call.as_ref().map(|e| &e.0), "list_secret_requests")?;
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let slots = state
        .secret_slots
        .list_open(auth.user_id(), conversation.id)
        .await?;
    Ok(Json(slots))
}

/// Provide the credential the assistant asked for.
#[utoipa::path(
    put, tag = "AI Chat", path = "/ai/conversations/{public_id}/secrets/{secret_id}",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("secret_id" = String, Path, description = "Secret request id"),
    ),
    request_body = FillSecretRequest,
    responses(
        (status = 200, description = "Credential stored; the response never includes it", body = SecretSlotView),
        (status = 400, description = "Empty or oversized value", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or secret request not found", body = ProblemDetails),
        (status = 409, description = "The request was cancelled or already closed", body = ProblemDetails),
        (status = 410, description = "The request expired", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn provide_secret(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<AiToolCall>>,
    Path((public_id, secret_id)): Path<(String, String)>,
    Json(request): Json<FillSecretRequest>,
) -> Result<Json<SecretSlotView>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    require_human_caller(ai_call.as_ref().map(|e| &e.0), "provide_secret")?;
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;

    let existing = state
        .secret_slots
        .get_owned(auth.user_id(), &secret_id)
        .await?;
    if existing.conversation_id != conversation.id {
        // Same answer as a slot that does not exist.
        return Err(SecretSlotError::NotFound {
            public_id: secret_id,
        }
        .into());
    }
    let slot = state
        .secret_slots
        .fill(auth.user_id(), &secret_id, &request.value)
        .await?;

    state
        .audit(&AiSecretProvidedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            secret_id: slot.id.clone(),
            operation_id: slot.operation_id.clone(),
            field: slot.field.clone(),
        })
        .await;
    Ok(Json(slot))
}

/// Discard a credential request and anything already typed into it.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/secrets/{secret_id}/discard",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("secret_id" = String, Path, description = "Secret request id"),
    ),
    responses(
        (status = 200, description = "Request discarded", body = SecretSlotView),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or secret request not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn discard_secret(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<AiToolCall>>,
    Path((public_id, secret_id)): Path<(String, String)>,
) -> Result<Json<SecretSlotView>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    require_human_caller(ai_call.as_ref().map(|e| &e.0), "discard_secret")?;
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;

    let existing = state
        .secret_slots
        .get_owned(auth.user_id(), &secret_id)
        .await?;
    if existing.conversation_id != conversation.id {
        return Err(SecretSlotError::NotFound {
            public_id: secret_id,
        }
        .into());
    }
    let slot = state
        .secret_slots
        .cancel(auth.user_id(), &secret_id)
        .await?;

    state
        .audit(&AiSecretDiscardedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            secret_id: slot.id.clone(),
            operation_id: slot.operation_id.clone(),
            field: slot.field.clone(),
        })
        .await;
    Ok(Json(slot))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/ai/conversations/{public_id}/secrets",
            get(list_secret_requests),
        )
        .route(
            "/ai/conversations/{public_id}/secrets/{secret_id}",
            put(provide_secret),
        )
        .route(
            "/ai/conversations/{public_id}/secrets/{secret_id}/discard",
            post(discard_secret),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(list_secret_requests, provide_secret, discard_secret),
    components(schemas(FillSecretRequest, SecretSlotView))
)]
pub struct SecretSlotsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_errors_map_to_the_status_a_client_can_act_on() {
        let status = |error: SecretSlotError| Problem::from(error).status_code;
        assert_eq!(
            status(SecretSlotError::NotFound {
                public_id: "x".into()
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(SecretSlotError::Validation {
                message: "empty".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(SecretSlotError::NotOpen {
                public_id: "x".into(),
                status: "cancelled".into()
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(SecretSlotError::Expired {
                public_id: "x".into(),
                expired_at: chrono::Utc::now()
            }),
            StatusCode::GONE
        );
    }

    #[test]
    fn the_fill_request_publishes_its_value_as_a_credential() {
        use utoipa::PartialSchema;
        let schema = serde_json::to_value(<FillSecretRequest as PartialSchema>::schema())
            .expect("serializes");
        assert_eq!(
            schema["properties"]["value"]["$ref"],
            "#/components/schemas/SecretString"
        );
    }
}
