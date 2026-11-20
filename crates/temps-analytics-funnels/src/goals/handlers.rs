// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface of conversion goals (ADR 109 D2), under
//! `/projects/{project_id}/analytics/goals`.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use serde::Serialize;
use temps_authz::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::{
    problemdetails::{self, Problem},
    AuditLogger, AuditOperation, RequestMetadata,
};
use utoipa::OpenApi;

use super::service::AnalyticsGoalService;
use super::types::*;
use crate::services::SmartFilter;

pub struct AnalyticsGoalsState {
    pub service: Arc<AnalyticsGoalService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(list_analytics_goals, create_analytics_goal, get_analytics_goal, update_analytics_goal, delete_analytics_goal, get_analytics_goal_stats, get_analytics_goal_breakdown),
    components(schemas(
        AnalyticsGoal, AnalyticsGoalInput, AnalyticsGoalKind, AnalyticsGoalListResponse, AnalyticsGoalStats, AnalyticsGoalStatsResponse, AnalyticsGoalRevenue,
        AnalyticsGoalBreakdownDimension, AnalyticsGoalBreakdownRow, AnalyticsGoalBreakdownResponse, SmartFilter
    )),
    tags((name = "Conversions", description = "Conversion goals: which events or pages count as a conversion, and how often visitors reach them"))
)]
pub struct AnalyticsGoalsApiDoc;

pub fn routes() -> Router<Arc<AnalyticsGoalsState>> {
    Router::new()
        .route(
            "/projects/{project_id}/analytics/goals",
            get(list_analytics_goals).post(create_analytics_goal),
        )
        .route(
            "/projects/{project_id}/analytics/goals/stats",
            get(get_analytics_goal_stats),
        )
        .route(
            "/projects/{project_id}/analytics/goals/{goal_id}",
            get(get_analytics_goal)
                .put(update_analytics_goal)
                .delete(delete_analytics_goal),
        )
        .route(
            "/projects/{project_id}/analytics/goals/{goal_id}/breakdown",
            get(get_analytics_goal_breakdown),
        )
}

impl From<AnalyticsGoalError> for Problem {
    fn from(error: AnalyticsGoalError) -> Self {
        match &error {
            AnalyticsGoalError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Conversion Goal Not Found")
                .with_detail(error.to_string()),
            AnalyticsGoalError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Conversion Goal")
                .with_detail(error.to_string()),
            AnalyticsGoalError::DuplicateName { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Conversion Goal Already Exists")
                .with_detail(error.to_string()),
            AnalyticsGoalError::LimitReached { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Too Many Conversion Goals")
                .with_detail(error.to_string()),
            AnalyticsGoalError::Database { .. } => {
                tracing::error!(error = %error, "Conversion goal request failed");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
            AnalyticsGoalError::Corrupt { .. } => {
                tracing::error!(error = %error, "Stored conversion goal is unreadable");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Unreadable Conversion Goal")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(Serialize)]
struct AnalyticsGoalAudit {
    operation: &'static str,
    project_id: i32,
    goal_id: i32,
    name: String,
    kind: AnalyticsGoalKind,
    user_id: Option<i32>,
    ip_address: String,
    user_agent: String,
}

impl AuditOperation for AnalyticsGoalAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }
    fn user_id(&self) -> Option<i32> {
        self.user_id
    }
    fn ip_address(&self) -> Option<String> {
        Some(self.ip_address.clone())
    }
    fn user_agent(&self) -> &str {
        &self.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

async fn record_audit(
    state: &AnalyticsGoalsState,
    operation: &'static str,
    goal: &AnalyticsGoal,
    user_id: Option<i32>,
    metadata: RequestMetadata,
) {
    let audit = AnalyticsGoalAudit {
        operation,
        project_id: goal.project_id,
        goal_id: goal.id,
        name: goal.name.clone(),
        kind: goal.kind,
        user_id,
        ip_address: metadata.ip_address,
        user_agent: metadata.user_agent,
    };
    if let Err(error) = state.audit.create_audit_log(&audit).await {
        tracing::error!(
            project_id = goal.project_id,
            goal_id = goal.id,
            operation,
            error = %error,
            "Failed to audit conversion goal change"
        );
    }
}

/// List a project's conversion goals, newest first
#[utoipa::path(
    get,
    path = "/projects/{project_id}/analytics/goals",
    tag = "Conversions",
    params(("project_id" = i32, Path, description = "Project ID"), AnalyticsGoalListQuery),
    responses(
        (status = 200, description = "Goals", body = AnalyticsGoalListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn list_analytics_goals(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsGoalsState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<AnalyticsGoalListQuery>,
) -> Result<Json<AnalyticsGoalListResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .service
            .list(project_id, query.page, query.page_size)
            .await?,
    ))
}

/// Define a conversion goal
#[utoipa::path(
    post,
    path = "/projects/{project_id}/analytics/goals",
    tag = "Conversions",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = AnalyticsGoalInput,
    responses(
        (status = 201, description = "Goal created", body = AnalyticsGoal),
        (status = 400, description = "Invalid goal"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Name taken or goal limit reached"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn create_analytics_goal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsGoalsState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(input): Json<AnalyticsGoalInput>,
) -> Result<(StatusCode, Json<AnalyticsGoal>), Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let goal = state.service.create(project_id, input).await?;
    record_audit(
        &state,
        "ANALYTICS_GOAL_CREATED",
        &goal,
        auth.user_id_opt(),
        metadata,
    )
    .await;
    Ok((StatusCode::CREATED, Json(goal)))
}

/// Get one conversion goal
#[utoipa::path(
    get,
    path = "/projects/{project_id}/analytics/goals/{goal_id}",
    tag = "Conversions",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("goal_id" = i32, Path, description = "Goal ID")
    ),
    responses(
        (status = 200, description = "Goal", body = AnalyticsGoal),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Goal not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_analytics_goal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsGoalsState>>,
    Path((project_id, goal_id)): Path<(i32, i32)>,
) -> Result<Json<AnalyticsGoal>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(state.service.get(project_id, goal_id).await?))
}

/// Replace a conversion goal's definition
#[utoipa::path(
    put,
    path = "/projects/{project_id}/analytics/goals/{goal_id}",
    tag = "Conversions",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("goal_id" = i32, Path, description = "Goal ID")
    ),
    request_body = AnalyticsGoalInput,
    responses(
        (status = 200, description = "Goal updated", body = AnalyticsGoal),
        (status = 400, description = "Invalid goal"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Goal not found"),
        (status = 409, description = "Name taken"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn update_analytics_goal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsGoalsState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, goal_id)): Path<(i32, i32)>,
    Json(input): Json<AnalyticsGoalInput>,
) -> Result<Json<AnalyticsGoal>, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let goal = state.service.update(project_id, goal_id, input).await?;
    record_audit(
        &state,
        "ANALYTICS_GOAL_UPDATED",
        &goal,
        auth.user_id_opt(),
        metadata,
    )
    .await;
    Ok(Json(goal))
}

/// Delete a conversion goal (stored events are not touched)
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/analytics/goals/{goal_id}",
    tag = "Conversions",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("goal_id" = i32, Path, description = "Goal ID")
    ),
    responses(
        (status = 204, description = "Goal deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Goal not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn delete_analytics_goal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsGoalsState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, goal_id)): Path<(i32, i32)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let goal = state.service.delete(project_id, goal_id).await?;
    record_audit(
        &state,
        "ANALYTICS_GOAL_DELETED",
        &goal,
        auth.user_id_opt(),
        metadata,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Conversions, conversion rate and revenue of every goal in a period
#[utoipa::path(
    get,
    path = "/projects/{project_id}/analytics/goals/stats",
    tag = "Conversions",
    params(("project_id" = i32, Path, description = "Project ID"), AnalyticsGoalStatsQuery),
    responses(
        (status = 200, description = "Conversion figures", body = AnalyticsGoalStatsResponse),
        (status = 400, description = "Invalid period"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_analytics_goal_stats(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsGoalsState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<AnalyticsGoalStatsQuery>,
) -> Result<Json<AnalyticsGoalStatsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .service
            .stats(
                project_id,
                query.start_date,
                query.end_date,
                query.environment_id,
            )
            .await?,
    ))
}

/// One goal's conversions by source, page, country or technology
#[utoipa::path(
    get,
    path = "/projects/{project_id}/analytics/goals/{goal_id}/breakdown",
    tag = "Conversions",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("goal_id" = i32, Path, description = "Goal ID"),
        AnalyticsGoalBreakdownQuery
    ),
    responses(
        (status = 200, description = "Breakdown", body = AnalyticsGoalBreakdownResponse),
        (status = 400, description = "Invalid period or limit"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Goal not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_analytics_goal_breakdown(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsGoalsState>>,
    Path((project_id, goal_id)): Path<(i32, i32)>,
    Query(query): Query<AnalyticsGoalBreakdownQuery>,
) -> Result<Json<AnalyticsGoalBreakdownResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state.service.breakdown(project_id, goal_id, query).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(error: AnalyticsGoalError) -> StatusCode {
        Problem::from(error).status_code
    }

    #[test]
    fn goal_errors_map_to_their_http_status() {
        assert_eq!(
            status(AnalyticsGoalError::NotFound {
                project_id: 1,
                goal_id: 2
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(AnalyticsGoalError::Validation {
                project_id: 1,
                message: "x".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(AnalyticsGoalError::DuplicateName {
                project_id: 1,
                name: "Signup".into()
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(AnalyticsGoalError::LimitReached {
                project_id: 1,
                limit: 50
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(AnalyticsGoalError::Corrupt {
                goal_id: 2,
                reason: "x".into()
            }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn not_found_names_the_goal_and_project() {
        let problem = Problem::from(AnalyticsGoalError::NotFound {
            project_id: 3,
            goal_id: 9,
        });
        let body = serde_json::to_value(&problem.body).expect("problem body");
        assert_eq!(body["detail"], "Conversion goal 9 not found in project 3");
    }
}
