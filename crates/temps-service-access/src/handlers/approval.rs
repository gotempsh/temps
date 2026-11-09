// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{handler::audit, AccessAppState};
use crate::{
    coordinator::CoordinatorError,
    service::FreshConsent,
    types::{ApproveServiceGrant, RequestServiceGrant, ServiceGrantApproval},
};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::sync::Arc;
use temps_authz::{
    deny_deployment_token, project_permission_guard, project_scope_guard, RequireAuth,
};
use temps_core::{
    problemdetails::{self, Problem},
    RequestMetadata,
};

impl From<CoordinatorError> for Problem {
    fn from(error: CoordinatorError) -> Self {
        let detail = error.to_string();
        let (status, title) = match error {
            CoordinatorError::Access(error) => return error.into(),
            CoordinatorError::Invalid { .. } => {
                (StatusCode::CONFLICT, "Sandbox service access changed")
            }
            CoordinatorError::Transport(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Sandbox worker could not confirm service access",
            ),
            CoordinatorError::Database { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Sandbox service access storage failed",
            ),
            CoordinatorError::Authority(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Sandbox service access authority failed",
            ),
            CoordinatorError::Plan(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Sandbox service access policy preparation failed",
            ),
            CoordinatorError::Delivery(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Sandbox service access delivery failed",
            ),
        };
        // Display for these domain errors contains operation/identity metadata,
        // never provider responses, stored keys or database driver messages.
        if status.is_server_error() {
            tracing::error!(operation = "approve_sandbox_service_grant", %detail, "Sandbox service access approval failed");
        }
        problemdetails::new(status)
            .with_title(title)
            .with_detail(detail)
    }
}

#[utoipa::path(post, path="/projects/{project_id}/sandboxes/{sandbox_id}/service-grants/approve", tag="Sandbox service access",
    params(("project_id" = i32, Path, description="Project ID"), ("sandbox_id" = String, Path, description="Sandbox ID")),
    request_body=ApproveServiceGrant,
    responses((status=202, description="Fresh approval persisted; worker enforcement is asynchronous", body=ServiceGrantApproval),
        (status=400, description="Invalid consent"), (status=401, description="Unauthorized"), (status=403, description="Forbidden"),
        (status=404, description="Scoped resource not found"), (status=409, description="Policy or sandbox changed"),
        (status=500, description="Approval storage failed"), (status=503, description="Worker unavailable")), security(("bearer_auth"=[])))]
pub async fn approve_service_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, sandbox_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    Json(request): Json<ApproveServiceGrant>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|marker| &marker.0),
        "approve_service_grant",
    )?;
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        &state.project_access_checker
    );
    project_permission_guard!(
        auth,
        SandboxesWrite,
        project_id,
        &state.project_access_checker
    );
    let consent = FreshConsent {
        request_id: request.request_id,
        project_id,
        sandbox_id: sandbox_id.clone(),
        user_id: auth.user_id(),
        admin: auth.is_admin(),
        request: RequestServiceGrant {
            service_id: request.service_id,
            service_version: request.service_version,
            ttl_seconds: request.ttl_seconds,
        },
    };
    consent.validate()?;
    let service = state
        .service
        .get_service(project_id, &consent.request.service_id)
        .await?;
    if service.policy.github_repository.is_some() {
        project_permission_guard!(
            auth,
            GitRepositoriesRead,
            project_id,
            &state.project_access_checker
        );
    }
    let receipt = state.coordinator.connect(consent).await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &receipt.grant_id,
        1,
        "SANDBOX_SERVICE_GRANT_APPROVED",
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(ServiceGrantApproval {
            grant_id: receipt.grant_id,
            sandbox_id,
            expires_at: receipt.expires_at,
        }),
    ))
}
