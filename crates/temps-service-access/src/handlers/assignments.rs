// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{handler::audit, AccessAppState};
use crate::{assignment_types::*, types::AccessPageQuery};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::sync::Arc;
use temps_authz::{
    deny_deployment_token, project_permission_guard, project_scope_guard, RequireAuth,
};
use temps_core::{problemdetails::Problem, RequestMetadata};

#[utoipa::path(get,path="/projects/{project_id}/sandboxes/{sandbox_id}/services",tag="Sandbox services",params(("project_id"=i32,Path),("sandbox_id"=String,Path), AccessPageQuery),
responses((status=200,description="Service access state; inspect enforcement for worker acknowledgment",body=SandboxServiceList),
(status=400,description="Invalid access request"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Scoped resource not found"),(status=409,description="Policy or approval changed"),(status=500,description="Storage failed")),security(("bearer_auth"=[])))]
pub async fn list_sandbox_services(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, sandbox_id)): Path<(i32, String)>,
    Query(query): Query<AccessPageQuery>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsRead,
        project_id,
        &state.project_access_checker
    );
    project_permission_guard!(
        auth,
        SandboxesRead,
        project_id,
        &state.project_access_checker
    );
    let response = state
        .service
        .list_sandbox_services(
            project_id,
            &sandbox_id,
            auth.user_id(),
            auth.is_admin(),
            query,
        )
        .await?;
    Ok(Json(response))
}

#[utoipa::path(post,path="/projects/{project_id}/sandboxes/{sandbox_id}/services",tag="Sandbox services",params(("project_id"=i32,Path),("sandbox_id"=String,Path)),
request_body=RequestSandboxService,
responses((status=202,description="Service access state; inspect enforcement for worker acknowledgment",body=SandboxServiceResponse),
(status=400,description="Invalid access request"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Scoped resource not found"),(status=409,description="Policy or approval changed"),(status=500,description="Storage failed")),security(("bearer_auth"=[])))]
pub async fn request_sandbox_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, sandbox_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RequestSandboxService>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
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
    let response = state
        .service
        .request_sandbox_service(
            project_id,
            &sandbox_id,
            auth.user_id(),
            auth.is_admin(),
            request,
        )
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "SANDBOX_SERVICE_ACCESS_REQUESTED",
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[utoipa::path(post,path="/projects/{project_id}/sandboxes/{sandbox_id}/services/{access_id}/approve",tag="Sandbox services",params(("project_id"=i32,Path),("sandbox_id"=String,Path), ("access_id"=String,Path)),
request_body=ApproveSandboxService,
responses((status=202,description="Service access state; inspect enforcement for worker acknowledgment",body=SandboxServiceResponse),
(status=400,description="Invalid access request"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Scoped resource not found"),(status=409,description="Policy or approval changed"),(status=500,description="Storage failed")),security(("bearer_auth"=[])))]
pub async fn approve_sandbox_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, sandbox_id, access_id)): Path<(i32, String, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ApproveSandboxService>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
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
    // Require repository permission before saving a durable Git approval. The
    // background planner rechecks the approver before issuing each policy lease.
    let service = state
        .service
        .assignment_service(
            project_id,
            &sandbox_id,
            &access_id,
            auth.user_id(),
            auth.is_admin(),
        )
        .await?;
    // The transaction must accept the same immutable policy version whose
    // permissions are checked here, never a caller's predicted future version.
    if service.version != request.service_version {
        return Err(crate::error::AccessError::Conflict {
            id: service.id,
            version: request.service_version,
        }
        .into());
    }
    if service.policy.github_repository.is_some() {
        project_permission_guard!(
            auth,
            GitRepositoriesRead,
            project_id,
            &state.project_access_checker
        );
        temps_authz::permission_guard!(auth, GitConnectionsRead);
    }
    let response = state
        .service
        .approve_sandbox_service(
            project_id,
            &sandbox_id,
            &access_id,
            auth.user_id(),
            auth.is_admin(),
            request,
        )
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "SANDBOX_SERVICE_ACCESS_APPROVED",
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[utoipa::path(post,path="/projects/{project_id}/sandboxes/{sandbox_id}/services/{access_id}/revoke",tag="Sandbox services",params(("project_id"=i32,Path),("sandbox_id"=String,Path), ("access_id"=String,Path)),
request_body=RevokeSandboxService,
responses((status=202,description="Service access state; inspect enforcement for worker acknowledgment",body=SandboxServiceResponse),
(status=400,description="Invalid access request"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Scoped resource not found"),(status=409,description="Policy or approval changed"),(status=500,description="Storage failed")),security(("bearer_auth"=[])))]
pub async fn revoke_sandbox_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, sandbox_id, access_id)): Path<(i32, String, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RevokeSandboxService>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
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
    let response = state
        .service
        .revoke_sandbox_service(
            project_id,
            &sandbox_id,
            &access_id,
            auth.user_id(),
            auth.is_admin(),
            request.expected_version,
        )
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "SANDBOX_SERVICE_ACCESS_REVOKED",
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(response)))
}
