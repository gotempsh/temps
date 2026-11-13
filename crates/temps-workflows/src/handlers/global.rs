// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::{WorkflowRunResponse, WorkflowsAppState};
use crate::{store::RunFilter, types::RunStatus};
use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_authz::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use utoipa::{IntoParams, ToSchema};

#[derive(Deserialize, IntoParams)]
pub struct GlobalRunsQuery {
    pub status: Option<RunStatus>,
    pub workflow: Option<String>,
    pub page_size: Option<u64>,
    pub cursor: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct GlobalWorkflowRunsResponse {
    pub runs: Vec<WorkflowRunResponse>,
    pub next_cursor: Option<String>,
}

#[utoipa::path(get, path = "/workflows/runs", tag = "Workflows", params(GlobalRunsQuery),
    responses((status = 200, body = GlobalWorkflowRunsResponse, description = "Authorized runs across projects, newest first"),
        (status = 400, description = "Invalid cursor"), (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"), (status = 500, description = "Access or database failure")),
    security(("bearer_auth" = [])))]
pub async fn list_all_workflow_runs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Query(query): Query<GlobalRunsQuery>,
) -> Result<Json<GlobalWorkflowRunsResponse>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, WorkflowsRead);
    let result = state
        .service
        .global_runs(
            &auth,
            state.project_access_checker.as_deref(),
            &RunFilter {
                status: query.status,
                workflow_name: query.workflow,
            },
            query.cursor.as_deref(),
            query.page_size.unwrap_or(20),
        )
        .await?;
    Ok(Json(GlobalWorkflowRunsResponse {
        runs: result.runs.into_iter().map(Into::into).collect(),
        next_cursor: result.next,
    }))
}
