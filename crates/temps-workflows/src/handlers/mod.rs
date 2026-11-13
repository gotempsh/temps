// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The workflows HTTP API (ADR-103 §10, §11).
//!
//! - **Management** (session or API key, `permission_guard`ed and narrowed
//!   to the project): under
//!   `/projects/{project_id}/environments/{environment_id}/workflows`.
//! - **Executor** (public routes; the invocation token Temps signed is the
//!   credential): `/workflow-runs/{run_id}/...` and the signing key at
//!   `/workflows/v1/invocation-key`.
//! - **App** (public routes; the environment's `twf_` workflow credential):
//!   `/workflows/v1/runs`, `/workflows/v1/events`.

mod app;
pub mod audit;
mod executor;
mod management;
#[cfg(test)]
mod tests;
pub mod types;

use std::sync::Arc;

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use temps_core::problemdetails::{self, Problem};
use temps_core::{PayloadBlobError, RequestMetadata};
use utoipa::OpenApi;

use crate::error::WorkflowError;
pub use types::WorkflowsAppState;
use types::*;

use app::*;
use executor::*;
use management::*;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_workflows_overview,
        list_workflow_runs,
        start_workflow_run,
        get_workflow_run,
        list_workflow_steps,
        reveal_workflow_payload,
        cancel_workflow_run,
        retry_workflow_run,
        resume_workflow_run,
        delete_workflow_run,
        send_workflow_event,
        get_workflow_settings,
        update_workflow_settings,
        get_workflow_credential,
        issue_workflow_credential,
        revoke_workflow_credential,
        get_invocation_key,
        get_run_journal,
        record_run_step,
        heartbeat_run,
        report_run_outcome,
        app_start_workflow_run,
        app_get_workflow_run,
        app_send_workflow_event,
    ),
    components(schemas(
        WorkflowsOverviewResponse,
        ExecutorCapabilityResponse,
        CredentialStatusResponse,
        WorkflowSummaryResponse,
        WorkflowRunResponse,
        AwaitedEventResponse,
        WorkflowRunListResponse,
        WorkflowStepResponse,
        WorkflowStepListResponse,
        StartWorkflowRunRequest,
        StartWorkflowRunResponse,
        RetryWorkflowRunRequest,
        RevealTarget,
        RevealPayloadRequest,
        RevealPayloadResponse,
        SendWorkflowEventRequest,
        SendWorkflowEventResponse,
        IssuedWorkflowCredentialResponse,
        AppWorkflowRunResponse,
        AppStartWorkflowRunResponse,
        crate::types::WorkflowSettings,
        crate::types::RunStatus,
        crate::types::StepKind,
        crate::types::StepStatus,
        crate::protocol::InvocationKeyResponse,
        crate::protocol::JournalEntry,
        crate::protocol::JournalPage,
        crate::protocol::StepWriteRequest,
        crate::protocol::StepWriteResponse,
        crate::protocol::EmittedEvent,
        crate::protocol::HeartbeatResponse,
        crate::protocol::Outcome,
        crate::protocol::OutcomeRequest,
        crate::protocol::OutcomeResponse,
    )),
    tags(
        (name = "Workflows", description = "Durable workflows: runs, steps, events and settings of an environment"),
        (name = "Workflow Executor", description = "Callbacks of the workflow SDK running in the app, authorized by the invocation token"),
        (name = "Workflow App API", description = "Start runs, read them and send events from the app, with the environment's workflow credential")
    )
)]
pub struct WorkflowsApiDoc;

const RUNS: &str = "/projects/{project_id}/environments/{environment_id}/workflows";

/// Authenticated management routes.
pub fn configure_routes() -> Router<Arc<WorkflowsAppState>> {
    Router::new()
        .route(RUNS, get(get_workflows_overview))
        .route(
            &format!("{RUNS}/runs"),
            get(list_workflow_runs).post(start_workflow_run),
        )
        .route(
            &format!("{RUNS}/runs/{{run_id}}"),
            get(get_workflow_run).delete(delete_workflow_run),
        )
        .route(
            &format!("{RUNS}/runs/{{run_id}}/steps"),
            get(list_workflow_steps),
        )
        .route(
            &format!("{RUNS}/runs/{{run_id}}/reveal"),
            post(reveal_workflow_payload),
        )
        .route(
            &format!("{RUNS}/runs/{{run_id}}/cancel"),
            post(cancel_workflow_run),
        )
        .route(
            &format!("{RUNS}/runs/{{run_id}}/retry"),
            post(retry_workflow_run),
        )
        .route(
            &format!("{RUNS}/runs/{{run_id}}/resume"),
            post(resume_workflow_run),
        )
        .route(&format!("{RUNS}/events"), post(send_workflow_event))
        .route(
            &format!("{RUNS}/settings"),
            get(get_workflow_settings).put(update_workflow_settings),
        )
        .route(
            &format!("{RUNS}/credential"),
            get(get_workflow_credential)
                .post(issue_workflow_credential)
                .delete(revoke_workflow_credential),
        )
}

/// Public routes: authenticated by the invocation token or the workflow
/// credential inside each handler, not by the session middleware.
pub fn configure_public_routes() -> Router<Arc<WorkflowsAppState>> {
    Router::new()
        .route("/workflows/v1/invocation-key", get(get_invocation_key))
        .route("/workflow-runs/{run_id}/journal", get(get_run_journal))
        .route("/workflow-runs/{run_id}/steps", post(record_run_step))
        .route("/workflow-runs/{run_id}/heartbeat", post(heartbeat_run))
        .route("/workflow-runs/{run_id}/outcome", post(report_run_outcome))
        .route("/workflows/v1/runs", post(app_start_workflow_run))
        .route("/workflows/v1/runs/{run_id}", get(app_get_workflow_run))
        .route("/workflows/v1/events", post(app_send_workflow_event))
}

impl From<WorkflowError> for Problem {
    fn from(error: WorkflowError) -> Self {
        let (status, title) = match &error {
            WorkflowError::EnvironmentNotFound { .. } => {
                (StatusCode::NOT_FOUND, "Environment Not Found")
            }
            WorkflowError::RunNotFound { .. } => (StatusCode::NOT_FOUND, "Workflow Run Not Found"),
            WorkflowError::StepNotFound { .. } => {
                (StatusCode::NOT_FOUND, "Workflow Step Not Found")
            }
            WorkflowError::Validation { .. } => (StatusCode::BAD_REQUEST, "Validation Error"),
            WorkflowError::PayloadTooLarge { .. } => {
                (StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large")
            }
            WorkflowError::QuotaExceeded { .. } => {
                (StatusCode::TOO_MANY_REQUESTS, "Workflow Quota Exceeded")
            }
            WorkflowError::InvalidState { .. } => {
                (StatusCode::CONFLICT, "Invalid Workflow Run State")
            }
            WorkflowError::StaleLease { .. } => (StatusCode::CONFLICT, "Stale Lease"),
            WorkflowError::Incompatible { .. } => (StatusCode::CONFLICT, "Workflow Incompatible"),
            WorkflowError::InvalidToken { .. } => {
                (StatusCode::UNAUTHORIZED, "Invalid Invocation Token")
            }
            WorkflowError::InvalidCredential { .. } => {
                (StatusCode::UNAUTHORIZED, "Invalid Workflow Credential")
            }
            WorkflowError::ExecutorUnavailable { .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Workflow Executor Unavailable",
            ),
            WorkflowError::Encryption { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Payload Encryption Failed",
            ),
            WorkflowError::Encoding { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Payload Encoding Failed")
            }
            WorkflowError::Blob(blob) => match blob {
                PayloadBlobError::NotFound { .. } => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "Payload Blob Missing")
                }
                PayloadBlobError::Unavailable { .. } => {
                    (StatusCode::SERVICE_UNAVAILABLE, "Blob Storage Unavailable")
                }
                PayloadBlobError::Operation { .. } => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "Blob Storage Error")
                }
            },
            WorkflowError::Endpoints(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Executor Endpoint Lookup Failed",
            ),
            WorkflowError::Invocation { .. } => (StatusCode::BAD_GATEWAY, "Invocation Failed"),
            WorkflowError::Database { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

/// The bearer token of a public request, or a 401.
fn bearer(headers: &HeaderMap) -> Result<&str, Problem> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            problemdetails::new(StatusCode::UNAUTHORIZED)
                .with_title("Missing Bearer Token")
                .with_detail("Send the token as `Authorization: Bearer <token>`")
        })
}

fn audit_context(
    user_id: Option<i32>,
    credential_id: Option<i32>,
    metadata: Option<&RequestMetadata>,
) -> audit::WorkflowAuditContext {
    audit::WorkflowAuditContext {
        user_id,
        credential_id,
        ip_address: metadata.map(|m| m.ip_address.clone()),
        user_agent: metadata.map(|m| m.user_agent.clone()).unwrap_or_default(),
    }
}

async fn record_audit(state: &WorkflowsAppState, operation: &dyn temps_core::AuditOperation) {
    if let Err(error) = state.audit_service.create_audit_log(operation).await {
        tracing::error!(
            operation = %operation.operation_type(),
            %error,
            "failed to write a workflow audit log entry"
        );
    }
}

/// A response that must not be cached (a revealed payload, a new credential).
fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn total_pages(total: u64, page_size: u64) -> u64 {
    total.div_ceil(page_size.max(1))
}
