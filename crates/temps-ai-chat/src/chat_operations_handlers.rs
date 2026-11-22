// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The endpoint behind a chat's operations button: the deployments, backups
//! and restores the chat started, with their live state.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use temps_auth::permissions::Permission;
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use utoipa::OpenApi;

use crate::chat_operations::{
    ChatOperation, ChatOperationError, ChatOperationKind, ChatOperationService, ChatOperationState,
    ChatOperationsResponse, OperationVisibility,
};
use crate::handlers::{application_project_access_map, ensure_user_conversation_access, AppState};

impl From<ChatOperationError> for Problem {
    fn from(error: ChatOperationError) -> Self {
        match error {
            ChatOperationError::Database { .. } | ChatOperationError::Record { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// The deployments, backups and restores this chat started, newest first, with
/// their current state. Only what the caller may still read is listed:
/// deployments of projects they can access, and backups and restores when they
/// can read backups.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/operations",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "Operations the conversation started", body = ChatOperationsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_chat_operations(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<Json<ChatOperationsResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;

    let operations = ChatOperationService::new(state.db.clone());
    let visibility = OperationVisibility {
        deployments: auth.has_permission(&Permission::DeploymentsRead),
        backups: auth.has_permission(&Permission::BackupsRead),
    };
    let (mut rows, truncated) = operations.recorded(&conversation, visibility).await?;

    // A deployment is shown only while its project is still visible to the
    // caller: the chat is theirs, but project membership can change.
    let mut project_ids: Vec<i32> = rows.iter().filter_map(|row| row.project_id).collect();
    project_ids.sort_unstable();
    project_ids.dedup();
    if !project_ids.is_empty() {
        if let Some(access) =
            application_project_access_map(&auth, &state.project_access_checker, &project_ids)
                .await?
        {
            rows.retain(|row| {
                row.project_id
                    .is_none_or(|project_id| access.get(&project_id).copied().unwrap_or(false))
            });
        }
    }

    Ok(Json(
        operations.resolve(&conversation, rows, truncated).await?,
    ))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/ai/conversations/{public_id}/operations",
        get(list_chat_operations),
    )
}

#[derive(OpenApi)]
#[openapi(
    paths(list_chat_operations),
    components(schemas(
        ChatOperation,
        ChatOperationKind,
        ChatOperationState,
        ChatOperationsResponse
    ))
)]
pub struct ChatOperationsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_lookup_is_an_internal_error_naming_the_chat() {
        let problem = Problem::from(ChatOperationError::Database {
            conversation_id: "conv_1".into(),
            reason: "connection closed".into(),
        });
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(problem
            .body
            .get("detail")
            .and_then(|detail| detail.as_str())
            .is_some_and(|detail| detail.contains("conv_1")));
    }
}
