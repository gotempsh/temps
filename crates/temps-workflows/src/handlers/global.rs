// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::{WorkflowRunResponse, WorkflowsAppState};
use crate::{
    service::{DefinitionState, WorkflowDefinition},
    store::RunFilter,
    types::RunStatus,
};
use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
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

/// A run in the cross-project listing: the run, plus the slugs that name its
/// project and environment, so the list reads `shop · production` rather than
/// bare ids.
#[derive(Serialize, ToSchema)]
pub struct GlobalWorkflowRunResponse {
    #[serde(flatten)]
    pub run: WorkflowRunResponse,
    pub project_slug: String,
    pub environment_slug: String,
}

#[derive(Serialize, ToSchema)]
pub struct GlobalWorkflowRunsResponse {
    pub runs: Vec<GlobalWorkflowRunResponse>,
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
    let runs = result
        .runs
        .into_iter()
        .map(|scanned| GlobalWorkflowRunResponse {
            run: scanned.run.into(),
            project_slug: scanned.project_slug,
            environment_slug: scanned.environment_slug,
        })
        .collect();
    Ok(Json(GlobalWorkflowRunsResponse {
        runs,
        next_cursor: result.next,
    }))
}

#[derive(Deserialize, IntoParams)]
pub struct WorkflowDefinitionsQuery {
    /// 1-based page; default 1.
    pub page: Option<u64>,
    /// Default 20, at most 100.
    pub page_size: Option<u64>,
}

/// Whether the environment's current executor declares the workflow.
#[derive(Serialize, ToSchema, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowDefinitionState {
    /// The deployed executor declares it.
    Available,
    /// Only its run history remains; the current deployment no longer declares it.
    Missing,
    /// The executor has not been asked since the server started; opening the
    /// environment's workflows asks it.
    Unknown,
}

impl From<DefinitionState> for WorkflowDefinitionState {
    fn from(state: DefinitionState) -> Self {
        match state {
            DefinitionState::Available => Self::Available,
            DefinitionState::Missing => Self::Missing,
            DefinitionState::Unknown => Self::Unknown,
        }
    }
}

/// One workflow in one environment, with its run counts.
#[derive(Serialize, ToSchema)]
pub struct WorkflowDefinitionResponse {
    pub name: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub project_slug: String,
    pub environment_slug: String,
    pub state: WorkflowDefinitionState,
    pub version: Option<String>,
    /// Runs allowed to execute at once, from the manifest.
    pub concurrency: Option<u32>,
    pub active_runs: i64,
    pub completed_runs: i64,
    pub failed_runs: i64,
    pub last_run_at: Option<DateTime<Utc>>,
}

impl From<WorkflowDefinition> for WorkflowDefinitionResponse {
    fn from(definition: WorkflowDefinition) -> Self {
        Self {
            name: definition.name,
            project_id: definition.environment.project_id,
            environment_id: definition.environment.environment_id,
            project_slug: definition.environment.project_slug,
            environment_slug: definition.environment.environment_slug,
            state: definition.state.into(),
            version: definition.version,
            concurrency: definition.concurrency,
            active_runs: definition.active_runs,
            completed_runs: definition.completed_runs,
            failed_runs: definition.failed_runs,
            last_run_at: definition.last_run_at,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct WorkflowDefinitionsResponse {
    pub workflows: Vec<WorkflowDefinitionResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[utoipa::path(get, path = "/workflows/definitions", tag = "Workflows", params(WorkflowDefinitionsQuery),
    responses((status = 200, body = WorkflowDefinitionsResponse, description = "Workflows across the projects the caller may read, most recently run first"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"), (status = 500, description = "Access or database failure")),
    security(("bearer_auth" = [])))]
pub async fn list_workflow_definitions(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Query(query): Query<WorkflowDefinitionsQuery>,
) -> Result<Json<WorkflowDefinitionsResponse>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, WorkflowsRead);
    let definitions = state
        .service
        .workflow_definitions(&auth, state.project_access_checker.as_deref())
        .await?;
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    let page = query.page.unwrap_or(1).max(1);
    let total = definitions.len() as u64;
    let workflows = definitions
        .into_iter()
        .skip(((page - 1) * page_size) as usize)
        .take(page_size as usize)
        .map(Into::into)
        .collect();
    Ok(Json(WorkflowDefinitionsResponse {
        workflows,
        total,
        page,
        page_size,
        total_pages: total.div_ceil(page_size),
    }))
}
