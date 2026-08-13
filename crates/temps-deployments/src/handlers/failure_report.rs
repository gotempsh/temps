//! Deploy-failure report handlers: preview a redacted, editable copy of a
//! failed job's trace, and send a user-reviewed copy to the Temps team.
//!
//! See `crate::services::failure_report_service` for the redaction and
//! delivery logic. This is deliberately separate from the anonymous
//! product-telemetry pipeline (`temps_core::telemetry`), which never
//! carries free-form user text.

use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::{problemdetails, AuditContext, RequestMetadata};
use tracing::error;

use super::audit::DeploymentFailureReportedAudit;
use super::types::{AppState, FailureReportPreviewResponse, SendFailureReportRequest};
use crate::services::FailureReportError;
use temps_core::problemdetails::Problem;

/// Preview a redacted, editable copy of a failed job's trace.
#[utoipa::path(
    get,
    tag = "Deployments",
    path = "/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/failure-report",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
        ("job_id" = String, Path, description = "Failed job ID")
    ),
    responses(
        (status = 200, description = "Redacted failure-report preview", body = FailureReportPreviewResponse),
        (status = 404, description = "Job not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_token" = []))
)]
pub async fn get_failure_report_preview(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, deployment_id, job_id)): Path<(i32, i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let preview = state
        .failure_report_service
        .build_preview(project_id, deployment_id, &job_id)
        .await?;

    Ok((
        StatusCode::OK,
        Json(FailureReportPreviewResponse::from(preview)),
    ))
}

/// Send a user-reviewed failure report to the Temps team.
#[utoipa::path(
    post,
    tag = "Deployments",
    path = "/projects/{project_id}/deployments/{deployment_id}/jobs/{job_id}/failure-report",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
        ("job_id" = String, Path, description = "Failed job ID")
    ),
    request_body = SendFailureReportRequest,
    responses(
        (status = 204, description = "Report sent"),
        (status = 404, description = "Job not found"),
        (status = 502, description = "Failed to reach the central reporting endpoint"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_token" = []))
)]
pub async fn send_failure_report(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, deployment_id, job_id)): Path<(i32, i32, String)>,
    Json(request): Json<SendFailureReportRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DeploymentsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    state
        .failure_report_service
        .send_report(project_id, deployment_id, &job_id, &request.report_text)
        .await?;

    let audit = DeploymentFailureReportedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        deployment_id,
        job_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

impl From<FailureReportError> for Problem {
    fn from(err: FailureReportError) -> Self {
        match err {
            FailureReportError::JobNotFound {
                deployment_id,
                job_id,
            } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Job Not Found")
                .with_detail(format!(
                    "Job '{job_id}' not found in deployment {deployment_id}"
                )),
            FailureReportError::LogRead { job_id, reason } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failed To Read Log")
                    .with_detail(format!("Failed to read log for job '{job_id}': {reason}"))
            }
            FailureReportError::HttpClient { reason } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Failure Report Client Error")
                    .with_detail(reason)
            }
            FailureReportError::SendFailed {
                deployment_id,
                reason,
            } => problemdetails::new(StatusCode::BAD_GATEWAY)
                .with_title("Failed To Send Failure Report")
                .with_detail(format!(
                    "Failed to send failure report for deployment {deployment_id}: {reason}"
                )),
            FailureReportError::Deployment(deployment_error) => deployment_error.into(),
        }
    }
}
