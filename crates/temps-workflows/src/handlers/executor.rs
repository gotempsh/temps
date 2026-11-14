// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The executor's callbacks. Public routes: the credential is the
//! invocation token Temps signed for one run and lease, verified by the
//! engine before anything is read or written.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::Json;
use temps_core::problemdetails::Problem;
use uuid::Uuid;

use super::types::*;
use super::{bearer, json_body};
use crate::protocol::{
    HeartbeatResponse, InvocationKeyResponse, JournalPage, OutcomeRequest, OutcomeResponse,
    StepWriteRequest, StepWriteResponse,
};

type AppState = State<Arc<WorkflowsAppState>>;

#[utoipa::path(
    tag = "Workflow Executor",
    get,
    path = "/workflows/v1/invocation-key",
    responses(
        (status = 200, description = "The public key SDKs verify invocation tokens with, and the project and environment the tokens must name", body = InvocationKeyResponse),
        (status = 401, description = "Missing, unknown or revoked workflow credential")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_invocation_key(
    State(state): AppState,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Problem> {
    let credential = state
        .service
        .authenticate_credential(bearer(&headers)?)
        .await?;
    let key = &state.invocation_key;
    Ok(Json(InvocationKeyResponse {
        algorithm: key.algorithm.clone(),
        public_key: key.public_key.clone(),
        key_id: key.key_id.clone(),
        project_id: credential.project_id,
        environment_id: credential.environment_id,
    }))
}

#[utoipa::path(
    tag = "Workflow Executor",
    get,
    path = "/workflow-runs/{run_id}/journal",
    params(("run_id" = Uuid, Path, description = "Run ID"), JournalQuery),
    responses(
        (status = 200, description = "A page of the journal, decrypted for the executor", body = JournalPage),
        (status = 401, description = "Missing or invalid invocation token"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The token's lease is no longer current")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_run_journal(
    State(state): AppState,
    headers: HeaderMap,
    Path(run_id): Path<Uuid>,
    Query(query): Query<JournalQuery>,
) -> Result<impl IntoResponse, Problem> {
    let token = bearer(&headers)?;
    let page = state
        .engine
        .journal_page(token, run_id, query.from.unwrap_or(0))
        .await?;
    Ok(Json(page))
}

#[utoipa::path(
    tag = "Workflow Executor",
    post,
    path = "/workflow-runs/{run_id}/steps",
    params(("run_id" = Uuid, Path, description = "Run ID")),
    request_body = StepWriteRequest,
    responses(
        (status = 200, description = "The entry as recorded; when already_recorded, continue with it", body = StepWriteResponse),
        (status = 400, description = "Invalid entry"),
        (status = 401, description = "Missing or invalid invocation token"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "Stale lease, or the replay does not match the journal (WorkflowIncompatible)"),
        (status = 413, description = "Result too large"),
        (status = 429, description = "The run reached its step or journal-size quota")
    ),
    security(("bearer_auth" = []))
)]
pub async fn record_run_step(
    State(state): AppState,
    headers: HeaderMap,
    Path(run_id): Path<Uuid>,
    body: Body,
) -> Result<impl IntoResponse, Problem> {
    let token = bearer(&headers)?;
    state.engine.verify_invocation_token(token, run_id)?;
    let write: StepWriteRequest = json_body(body).await?;
    let response = state.engine.record_step(token, run_id, write).await?;
    Ok(Json(response))
}

#[utoipa::path(
    tag = "Workflow Executor",
    post,
    path = "/workflow-runs/{run_id}/heartbeat",
    params(("run_id" = Uuid, Path, description = "Run ID")),
    responses(
        (status = 200, description = "Lease extended, or `cancelled: true` to stop", body = HeartbeatResponse),
        (status = 401, description = "Missing or invalid invocation token"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The token's lease is no longer current")
    ),
    security(("bearer_auth" = []))
)]
pub async fn heartbeat_run(
    State(state): AppState,
    headers: HeaderMap,
    Path(run_id): Path<Uuid>,
) -> Result<impl IntoResponse, Problem> {
    let token = bearer(&headers)?;
    Ok(Json(state.engine.heartbeat(token, run_id).await?))
}

#[utoipa::path(
    tag = "Workflow Executor",
    post,
    path = "/workflow-runs/{run_id}/outcome",
    params(("run_id" = Uuid, Path, description = "Run ID")),
    request_body = OutcomeRequest,
    responses(
        (status = 200, description = "Outcome applied (for executors that answered the invoke with 202)", body = OutcomeResponse),
        (status = 400, description = "Invalid outcome"),
        (status = 401, description = "Missing or invalid invocation token"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The token's lease is no longer current")
    ),
    security(("bearer_auth" = []))
)]
pub async fn report_run_outcome(
    State(state): AppState,
    headers: HeaderMap,
    Path(run_id): Path<Uuid>,
    body: Body,
) -> Result<impl IntoResponse, Problem> {
    let token = bearer(&headers)?;
    state.engine.verify_invocation_token(token, run_id)?;
    let request: OutcomeRequest = json_body(body).await?;
    Ok(Json(
        state.engine.report_outcome(token, run_id, request).await?,
    ))
}
