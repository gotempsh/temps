// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Management handlers: every one requires a session or API key (never a
//! deployment token), checks its workflow permission narrowed to the
//! project, and audits its writes.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use temps_authz::{
    deny_deployment_token, project_permission_guard, project_scope_guard, RequireAuth,
};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use uuid::Uuid;

use super::audit::*;
use super::types::*;
use super::{audit_context, json_body, no_store, record_audit, total_pages};
use crate::error::WorkflowError;
use crate::service::{page_bounds, RevealField, StartRun};
use crate::store::{RunFilter, Starter};
use crate::types::WorkflowSettings;

type AppState = State<Arc<WorkflowsAppState>>;

fn starter(auth: &temps_authz::AuthContext) -> Starter {
    auth.user_id_opt().map_or(Starter::Machine, Starter::User)
}

#[utoipa::path(
    tag = "Workflows",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/workflows",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "Workflows of the environment, whether it can run them, and its credential status", body = WorkflowsOverviewResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_workflows_overview(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    let overview = state.service.overview(project_id, environment_id).await?;
    Ok(Json(WorkflowsOverviewResponse::from(overview)))
}

#[utoipa::path(
    tag = "Workflows",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ListRunsQuery
    ),
    responses(
        (status = 200, description = "Runs, newest first", body = WorkflowRunListResponse),
        (status = 400, description = "Invalid filter"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_workflow_runs(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Query(query): Query<ListRunsQuery>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    let (page, page_size) = page_bounds(query.page, query.page_size);
    let filter = RunFilter {
        status: query.status,
        workflow_name: query.workflow,
    };
    let (runs, total) = state
        .service
        .list_runs(project_id, environment_id, &filter, page, page_size)
        .await?;
    Ok(Json(WorkflowRunListResponse {
        runs: runs.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

#[utoipa::path(
    tag = "Workflows",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = StartWorkflowRunRequest,
    responses(
        (status = 201, description = "Run started", body = StartWorkflowRunResponse),
        (status = 200, description = "The idempotency key matched an earlier run", body = StartWorkflowRunResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment not found"),
        (status = 413, description = "Input too large"),
        (status = 429, description = "Active run quota reached"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_workflow_run(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    body: Body,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowRunsCreate,
        project_id,
        &state.project_access_checker
    );
    let request: StartWorkflowRunRequest = json_body(body).await?;
    let (run, created) = state
        .service
        .start(
            project_id,
            environment_id,
            StartRun {
                workflow: request.workflow,
                input: request.input,
                idempotency_key: request.idempotency_key,
            },
            starter(&auth),
        )
        .await?;
    if created {
        record_audit(
            &state,
            &WorkflowRunStartedAudit {
                context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
                project_id,
                environment_id,
                run_id: run.id,
                workflow: run.workflow_name.clone(),
            },
        )
        .await;
    }
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(StartWorkflowRunResponse {
            run: run.into(),
            created,
        }),
    ))
}

#[utoipa::path(
    tag = "Workflows",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs/{run_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = Uuid, Path, description = "Run ID")
    ),
    responses(
        (status = 200, description = "The run, payloads masked", body = WorkflowRunResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_workflow_run(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, Uuid)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    let run = state
        .service
        .get_run(project_id, environment_id, run_id)
        .await?;
    Ok(Json(WorkflowRunResponse::from(run)))
}

#[utoipa::path(
    tag = "Workflows",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs/{run_id}/diagnose",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = Uuid, Path, description = "Run ID")
    ),
    responses(
        (status = 200, description = "Run diagnosis without decrypted payloads", body = crate::service::WorkflowDiagnosis),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn diagnose_workflow_run(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, Uuid)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    let run = state
        .service
        .diagnose(project_id, environment_id, run_id)
        .await?;
    Ok(Json(run))
}

#[utoipa::path(
    tag = "Workflows",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs/{run_id}/steps",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = Uuid, Path, description = "Run ID"),
        PageQuery
    ),
    responses(
        (status = 200, description = "The run's journal in sequence order, payloads masked", body = WorkflowStepListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_workflow_steps(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, Uuid)>,
    Query(query): Query<PageQuery>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    let (page, page_size) = page_bounds(query.page, query.page_size);
    let (steps, total) = state
        .service
        .list_steps(project_id, environment_id, run_id, page, page_size)
        .await?;
    Ok(Json(WorkflowStepListResponse {
        steps: steps.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

#[utoipa::path(
    tag = "Workflows",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs/{run_id}/reveal",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = Uuid, Path, description = "Run ID")
    ),
    request_body = RevealPayloadRequest,
    responses(
        (status = 200, description = "The decrypted payload; the read is audit-logged", body = RevealPayloadResponse),
        (status = 400, description = "A step payload needs its sequence"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run or step not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn reveal_workflow_payload(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, Uuid)>,
    Json(request): Json<RevealPayloadRequest>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowPayloadsRead,
        project_id,
        &state.project_access_checker
    );
    let step = |sequence: Option<i32>| {
        sequence.ok_or(WorkflowError::Validation {
            field: "sequence",
            reason: "a step payload needs the step's sequence".into(),
        })
    };
    let field = match request.target {
        RevealTarget::Input => RevealField::Input,
        RevealTarget::Output => RevealField::Output,
        RevealTarget::Error => RevealField::Error,
        RevealTarget::StepResult => RevealField::StepResult(step(request.sequence)?),
        RevealTarget::StepError => RevealField::StepError(step(request.sequence)?),
    };
    let value = state
        .service
        .reveal(project_id, environment_id, run_id, field)
        .await?;
    record_audit(
        &state,
        &WorkflowPayloadRevealedAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            run_id,
            payload: field.label(),
        },
    )
    .await;
    Ok(no_store(Json(RevealPayloadResponse {
        target: request.target,
        sequence: request.sequence,
        value,
    })))
}

async fn audit_change(
    state: &WorkflowsAppState,
    auth: &temps_authz::AuthContext,
    metadata: &RequestMetadata,
    before: &crate::store::RunRow,
    after: &crate::store::RunRow,
    from_sequence: Option<i32>,
) {
    record_audit(
        state,
        &WorkflowRunChangedAudit {
            context: audit_context(auth.user_id_opt(), None, Some(metadata)),
            project_id: after.project_id,
            environment_id: after.environment_id,
            run_id: after.id,
            workflow: after.workflow_name.clone(),
            previous_status: before.status.clone(),
            new_status: after.status.clone(),
            from_sequence,
        },
    )
    .await;
}

#[utoipa::path(
    tag = "Workflows",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs/{run_id}/cancel",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = Uuid, Path, description = "Run ID")
    ),
    responses(
        (status = 200, description = "Run cancelled; an in-flight invocation stops at its next step", body = WorkflowRunResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The run already finished"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_workflow_run(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, Uuid)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowRunsWrite,
        project_id,
        &state.project_access_checker
    );
    let before = state
        .service
        .get_run(project_id, environment_id, run_id)
        .await?;
    let run = state
        .service
        .cancel(project_id, environment_id, run_id)
        .await?;
    audit_change(&state, &auth, &metadata, &before, &run, None).await;
    Ok(Json(WorkflowRunResponse::from(run)))
}

#[utoipa::path(
    tag = "Workflows",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs/{run_id}/retry",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = Uuid, Path, description = "Run ID")
    ),
    request_body = RetryWorkflowRunRequest,
    responses(
        (status = 200, description = "Run queued again from the given journal entry", body = WorkflowRunResponse),
        (status = 400, description = "Sequence outside the journal"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The run is still active"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn retry_workflow_run(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, Uuid)>,
    request: Option<Json<RetryWorkflowRunRequest>>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowRunsWrite,
        project_id,
        &state.project_access_checker
    );
    let from_sequence = request.and_then(|Json(request)| request.from_sequence);
    let before = state
        .service
        .get_run(project_id, environment_id, run_id)
        .await?;
    let run = state
        .service
        .retry_from(project_id, environment_id, run_id, from_sequence)
        .await?;
    audit_change(
        &state,
        &auth,
        &metadata,
        &before,
        &run,
        Some(run.step_count),
    )
    .await;
    Ok(Json(WorkflowRunResponse::from(run)))
}

#[utoipa::path(
    tag = "Workflows",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs/{run_id}/resume",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = Uuid, Path, description = "Run ID")
    ),
    responses(
        (status = 200, description = "Run due now; a pending sleep wakes now", body = WorkflowRunResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The run cannot be resumed in its state"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn resume_workflow_run(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, Uuid)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowRunsWrite,
        project_id,
        &state.project_access_checker
    );
    let before = state
        .service
        .get_run(project_id, environment_id, run_id)
        .await?;
    let run = state
        .service
        .resume(project_id, environment_id, run_id)
        .await?;
    audit_change(&state, &auth, &metadata, &before, &run, None).await;
    Ok(Json(WorkflowRunResponse::from(run)))
}

#[utoipa::path(
    tag = "Workflows",
    delete,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/runs/{run_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = Uuid, Path, description = "Run ID")
    ),
    responses(
        (status = 204, description = "Run and its journal deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The run is running; cancel it first"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_workflow_run(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, Uuid)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowRunsDelete,
        project_id,
        &state.project_access_checker
    );
    let run = state
        .service
        .delete(project_id, environment_id, run_id)
        .await?;
    record_audit(
        &state,
        &WorkflowRunDeletedAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            run_id,
            workflow: run.workflow_name,
            status: run.status,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Workflows",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/events",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = SendWorkflowEventRequest,
    responses(
        (status = 200, description = "Event stored and delivered to the runs waiting on it", body = SendWorkflowEventResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment not found"),
        (status = 413, description = "Payload too large"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn send_workflow_event(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(request): Json<SendWorkflowEventRequest>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowEventsCreate,
        project_id,
        &state.project_access_checker
    );
    let (event_id, delivery) = state
        .service
        .send_event(
            project_id,
            environment_id,
            &request.name,
            request.key.as_deref(),
            request.payload.as_ref(),
            "api",
        )
        .await?;
    record_audit(
        &state,
        &WorkflowEventSentAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            event_id,
            event_name: request.name,
            delivered: delivery.delivered,
        },
    )
    .await;
    Ok(Json(SendWorkflowEventResponse {
        event_id,
        delivered: delivery.delivered,
    }))
}

#[utoipa::path(
    tag = "Workflows",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/settings",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "Settings in effect (the defaults until changed)", body = WorkflowSettings),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_workflow_settings(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    Ok(Json(
        state.service.settings(project_id, environment_id).await?,
    ))
}

#[utoipa::path(
    tag = "Workflows",
    put,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/settings",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = WorkflowSettings,
    responses(
        (status = 200, description = "Settings saved", body = WorkflowSettings),
        (status = 400, description = "A value is out of range"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_workflow_settings(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(settings): Json<WorkflowSettings>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    let old = state.service.settings(project_id, environment_id).await?;
    let saved = state
        .service
        .update_settings(project_id, environment_id, settings)
        .await?;
    record_audit(
        &state,
        &WorkflowSettingsUpdatedAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            old,
            new: saved.clone(),
        },
    )
    .await;
    Ok(Json(saved))
}

#[utoipa::path(
    tag = "Workflows",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/credential",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "Whether the environment has a workflow credential, never its token", body = CredentialStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_workflow_credential(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    let credential = state.service.credential(project_id, environment_id).await?;
    Ok(Json(CredentialStatusResponse::from(credential)))
}

#[utoipa::path(
    tag = "Workflows",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/credential",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 201, description = "New credential; the token is shown only in this response. The previous credential stops working.", body = IssuedWorkflowCredentialResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn issue_workflow_credential(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    let issued = state
        .service
        .rotate_credential(project_id, environment_id, auth.user_id_opt())
        .await?;
    record_audit(
        &state,
        &WorkflowCredentialIssuedAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            credential_id: issued.credential.id,
            prefix: issued.credential.token_prefix.clone(),
        },
    )
    .await;
    Ok(no_store((
        StatusCode::CREATED,
        Json(IssuedWorkflowCredentialResponse {
            prefix: issued.credential.token_prefix.clone(),
            created_at: issued.credential.created_at,
            usage: "Set it as TEMPS_WORKFLOWS_TOKEN in the services that start runs or send \
                    events; send it as `Authorization: Bearer <token>` to /api/workflows/v1."
                .to_string(),
            token: issued.token,
        }),
    )))
}

#[utoipa::path(
    tag = "Workflows",
    delete,
    path = "/projects/{project_id}/environments/{environment_id}/workflows/credential",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 204, description = "Credential revoked"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Environment or credential not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_workflow_credential(
    RequireAuth(auth): RequireAuth,
    State(state): AppState,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    if !state
        .service
        .revoke_credential(project_id, environment_id)
        .await?
    {
        return Err(temps_core::error_builder::not_found()
            .detail(format!(
                "Environment {environment_id} of project {project_id} has no workflow credential to revoke"
            ))
            .build());
    }
    record_audit(
        &state,
        &WorkflowCredentialRevokedAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
