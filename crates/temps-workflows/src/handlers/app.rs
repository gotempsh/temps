// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The app-facing API: start a run, read it, send an event, with the
//! environment's `twf_` workflow credential. Public routes: the credential is
//! checked here, and scopes every call to its own project and environment.

use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use uuid::Uuid;

use super::audit::*;
use super::types::*;
use super::{audit_context, bearer, record_audit};
use crate::service::StartRun;
use crate::store::{CredentialRow, RunRow, Starter};
use crate::types::RunStatus;

type AppState = State<Arc<WorkflowsAppState>>;

async fn credential(
    state: &WorkflowsAppState,
    headers: &HeaderMap,
) -> Result<CredentialRow, Problem> {
    let token = bearer(headers)?;
    Ok(state.service.authenticate_credential(token).await?)
}

fn app_run(
    run: RunRow,
    output: Option<serde_json::Value>,
    error: Option<String>,
) -> AppWorkflowRunResponse {
    AppWorkflowRunResponse {
        id: run.id,
        workflow: run.workflow_name,
        status: RunStatus::parse(&run.status).unwrap_or(RunStatus::Failed),
        error_kind: run.error_kind,
        output,
        error,
        created_at: run.created_at,
        finished_at: run.finished_at,
    }
}

#[utoipa::path(
    tag = "Workflow App API",
    post,
    path = "/workflows/v1/runs",
    request_body = StartWorkflowRunRequest,
    responses(
        (status = 201, description = "Run started", body = AppStartWorkflowRunResponse),
        (status = 200, description = "The idempotency key matched an earlier run", body = AppStartWorkflowRunResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Missing, unknown or revoked workflow credential"),
        (status = 413, description = "Input too large"),
        (status = 429, description = "Active run quota reached")
    ),
    security(("bearer_auth" = []))
)]
pub async fn app_start_workflow_run(
    State(state): AppState,
    metadata: Option<Extension<RequestMetadata>>,
    headers: HeaderMap,
    Json(request): Json<StartWorkflowRunRequest>,
) -> Result<impl IntoResponse, Problem> {
    let credential = credential(&state, &headers).await?;
    let (run, created) = state
        .service
        .start(
            credential.project_id,
            credential.environment_id,
            StartRun {
                workflow: request.workflow,
                input: request.input,
                idempotency_key: request.idempotency_key,
            },
            Starter::Credential(credential.id),
        )
        .await?;
    if created {
        record_audit(
            &state,
            &WorkflowRunStartedAudit {
                context: audit_context(
                    None,
                    Some(credential.id),
                    metadata.as_ref().map(|Extension(m)| m),
                ),
                project_id: credential.project_id,
                environment_id: credential.environment_id,
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
        Json(AppStartWorkflowRunResponse {
            run: app_run(run, None, None),
            created,
        }),
    ))
}

#[utoipa::path(
    tag = "Workflow App API",
    get,
    path = "/workflows/v1/runs/{run_id}",
    params(("run_id" = Uuid, Path, description = "Run ID")),
    responses(
        (status = 200, description = "The run, with its output once completed or its error once failed", body = AppWorkflowRunResponse),
        (status = 401, description = "Missing, unknown or revoked workflow credential"),
        (status = 404, description = "No such run in the credential's environment")
    ),
    security(("bearer_auth" = []))
)]
pub async fn app_get_workflow_run(
    State(state): AppState,
    headers: HeaderMap,
    Path(run_id): Path<Uuid>,
) -> Result<impl IntoResponse, Problem> {
    let credential = credential(&state, &headers).await?;
    let (run, output, error) = state
        .service
        .run_result(credential.project_id, credential.environment_id, run_id)
        .await?;
    Ok(super::no_store(Json(app_run(run, output, error))))
}

#[utoipa::path(
    tag = "Workflow App API",
    post,
    path = "/workflows/v1/events",
    request_body = SendWorkflowEventRequest,
    responses(
        (status = 200, description = "Event stored and delivered to the runs waiting on it", body = SendWorkflowEventResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Missing, unknown or revoked workflow credential"),
        (status = 413, description = "Payload too large")
    ),
    security(("bearer_auth" = []))
)]
pub async fn app_send_workflow_event(
    State(state): AppState,
    metadata: Option<Extension<RequestMetadata>>,
    headers: HeaderMap,
    Json(request): Json<SendWorkflowEventRequest>,
) -> Result<impl IntoResponse, Problem> {
    let credential = credential(&state, &headers).await?;
    let (event_id, delivery) = state
        .service
        .send_event(
            credential.project_id,
            credential.environment_id,
            &request.name,
            request.key.as_deref(),
            request.payload.as_ref(),
            "credential",
        )
        .await?;
    record_audit(
        &state,
        &WorkflowEventSentAudit {
            context: audit_context(
                None,
                Some(credential.id),
                metadata.as_ref().map(|Extension(m)| m),
            ),
            project_id: credential.project_id,
            environment_id: credential.environment_id,
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
