// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Endpoints behind the assistant's goal panel: see what a chat is watching,
//! stop watching something, or give a stuck goal a fresh budget.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use temps_auth::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, RequestMetadata,
};
use utoipa::OpenApi;

use crate::audit::{AiGoalCancelledAudit, AiGoalRetriedAudit};
use crate::goals::{GoalDeployment, GoalError, GoalResponse};
use crate::handlers::{ensure_user_conversation_access, AppState};

impl From<GoalError> for Problem {
    fn from(error: GoalError) -> Self {
        match error {
            GoalError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Goal Not Found")
                .with_detail(error.to_string()),
            GoalError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Goal")
                .with_detail(error.to_string()),
            GoalError::TooManyOpen { .. } | GoalError::InvalidState { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Goal Cannot Change")
                    .with_detail(error.to_string())
            }
            GoalError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
        }
    }
}

/// List the goals a chat is watching or has finished with, newest first.
#[utoipa::path(
    get, tag = "AI Chat", path = "/ai/conversations/{public_id}/goals",
    params(("public_id" = String, Path, description = "Conversation public id")),
    responses(
        (status = 200, description = "Goals of the conversation", body = Vec<GoalResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_goals(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
) -> Result<Json<Vec<GoalResponse>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let goals = state.goals.list(conversation.id).await?;
    Ok(Json(goals.into_iter().map(GoalResponse::from).collect()))
}

/// Stop watching a goal. The check never runs again and no fix turn starts.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/goals/{goal_id}/cancel",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("goal_id" = String, Path, description = "Goal id"),
    ),
    responses(
        (status = 200, description = "Goal cancelled", body = GoalResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or goal not found", body = ProblemDetails),
        (status = 409, description = "Goal is already finished", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_goal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, goal_id)): Path<(String, String)>,
) -> Result<Json<GoalResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let goal = state.goals.cancel(conversation.id, &goal_id).await?;
    state
        .audit(&AiGoalCancelledAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            goal_id: goal.public_id.clone(),
        })
        .await;
    Ok(Json(GoalResponse::from(goal)))
}

/// Give a failed or waiting goal a fresh attempt budget and check it again now.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/goals/{goal_id}/retry",
    params(
        ("public_id" = String, Path, description = "Conversation public id"),
        ("goal_id" = String, Path, description = "Goal id"),
    ),
    responses(
        (status = 200, description = "Goal reopened", body = GoalResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Conversation or goal not found", body = ProblemDetails),
        (status = 409, description = "Goal is not failed or waiting", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn retry_goal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((public_id, goal_id)): Path<(String, String)>,
) -> Result<Json<GoalResponse>, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let goal = state.goals.retry(conversation.id, &goal_id).await?;
    state
        .audit(&AiGoalRetriedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            goal_id: goal.public_id.clone(),
        })
        .await;
    Ok(Json(GoalResponse::from(goal)))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ai/conversations/{public_id}/goals", get(list_goals))
        .route(
            "/ai/conversations/{public_id}/goals/{goal_id}/cancel",
            post(cancel_goal),
        )
        .route(
            "/ai/conversations/{public_id}/goals/{goal_id}/retry",
            post(retry_goal),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(list_goals, cancel_goal, retry_goal),
    components(schemas(GoalResponse, GoalDeployment))
)]
pub struct GoalsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_errors_map_to_the_status_a_client_can_act_on() {
        let status = |error: GoalError| Problem::from(error).status_code;
        assert_eq!(
            status(GoalError::NotFound {
                goal_id: "goal_x".into(),
                conversation_id: 1
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(GoalError::Validation {
                conversation_id: 1,
                message: "bad".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(GoalError::InvalidState {
                goal_id: "goal_x".into(),
                status: "achieved".into(),
                action: "cancelled"
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(GoalError::TooManyOpen {
                conversation_id: 1,
                open: 5,
                limit: 5
            }),
            StatusCode::CONFLICT
        );
    }
}
