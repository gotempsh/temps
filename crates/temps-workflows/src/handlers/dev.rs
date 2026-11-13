// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{audit::WorkflowDevAudit, audit_context, no_store, record_audit, WorkflowsAppState};
use crate::{
    dev::{DevPollResponse, DevReply, DevSessionResponse},
    protocol::Manifest,
};
use axum::{
    body::Body,
    extract::{Extension, Path, State},
    http::StatusCode,
    response::Response,
    Json,
};
use std::sync::Arc;
use temps_authz::{
    deny_deployment_token, project_permission_guard, project_scope_guard, RequireAuth,
};
use temps_core::{problemdetails::Problem, RequestMetadata};
use uuid::Uuid;

#[utoipa::path(post,path="/projects/{project_id}/environments/{environment_id}/workflows/dev",tag="Workflows",params(("project_id"=i32,Path),("environment_id"=i32,Path)),
responses((status=201,description="Development tunnel operation completed",body=DevSessionResponse),
(status=400,description="Invalid request"),(status=401,description="Unauthorized"),(status=403,description="Requires workflow write and payload access"),(status=409,description="Session unavailable"),(status=413,description="Message too large"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn open_workflow_dev(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<Response, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    project_permission_guard!(
        auth,
        WorkflowPayloadsRead,
        project_id,
        &state.project_access_checker
    );
    let result = state.service.open_dev(project_id, environment_id).await?;
    let session_id = result.session_id;
    record_audit(
        &state,
        &WorkflowDevAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            session_id,
            operation: "open_workflow_dev".into(),
        },
    )
    .await;
    Ok(no_store((StatusCode::CREATED, Json(result))))
}

#[utoipa::path(put,path="/projects/{project_id}/environments/{environment_id}/workflows/dev/{session_id}",tag="Workflows",params(("project_id"=i32,Path),("environment_id"=i32,Path),("session_id"=Uuid,Path)),request_body=Manifest,
responses((status=204,description="Development tunnel operation completed"),
(status=400,description="Invalid request"),(status=401,description="Unauthorized"),(status=403,description="Requires workflow write and payload access"),(status=409,description="Session unavailable"),(status=413,description="Message too large"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn configure_workflow_dev(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, session_id)): Path<(i32, i32, Uuid)>,
    body: Body,
) -> Result<Response, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    project_permission_guard!(
        auth,
        WorkflowPayloadsRead,
        project_id,
        &state.project_access_checker
    );
    let bytes = axum::body::to_bytes(body, crate::dev::DEV_BODY_LIMIT)
        .await
        .map_err(|_| {
            temps_core::problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                .with_title("Development tunnel message too large")
                .with_detail("Development tunnel messages are limited to 8 MiB")
        })?;
    let request: Manifest = serde_json::from_slice(&bytes).map_err(|_| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid development message")
            .with_detail("Expected the documented JSON request shape")
    })?;
    state
        .service
        .configure_dev(project_id, environment_id, session_id, request)
        .await?;

    record_audit(
        &state,
        &WorkflowDevAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            session_id,
            operation: "configure_workflow_dev".into(),
        },
    )
    .await;
    Ok(no_store(StatusCode::NO_CONTENT))
}

#[utoipa::path(delete,path="/projects/{project_id}/environments/{environment_id}/workflows/dev/{session_id}",tag="Workflows",params(("project_id"=i32,Path),("environment_id"=i32,Path),("session_id"=Uuid,Path)),
responses((status=204,description="Development tunnel operation completed"),
(status=400,description="Invalid request"),(status=401,description="Unauthorized"),(status=403,description="Requires workflow write and payload access"),(status=409,description="Session unavailable"),(status=413,description="Message too large"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn close_workflow_dev(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, session_id)): Path<(i32, i32, Uuid)>,
) -> Result<Response, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    project_permission_guard!(
        auth,
        WorkflowPayloadsRead,
        project_id,
        &state.project_access_checker
    );
    state
        .service
        .close_dev(project_id, environment_id, session_id)
        .await?;

    record_audit(
        &state,
        &WorkflowDevAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            session_id,
            operation: "close_workflow_dev".into(),
        },
    )
    .await;
    Ok(no_store(StatusCode::NO_CONTENT))
}

#[utoipa::path(post,path="/projects/{project_id}/environments/{environment_id}/workflows/dev/{session_id}/poll",tag="Workflows",params(("project_id"=i32,Path),("environment_id"=i32,Path),("session_id"=Uuid,Path)),
responses((status=200,description="Development tunnel operation completed",body=DevPollResponse),
(status=400,description="Invalid request"),(status=401,description="Unauthorized"),(status=403,description="Requires workflow write and payload access"),(status=409,description="Session unavailable"),(status=413,description="Message too large"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn poll_workflow_dev(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, session_id)): Path<(i32, i32, Uuid)>,
) -> Result<Response, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    project_permission_guard!(
        auth,
        WorkflowPayloadsRead,
        project_id,
        &state.project_access_checker
    );
    let result = state
        .service
        .poll_dev(project_id, environment_id, session_id)
        .await?;

    record_audit(
        &state,
        &WorkflowDevAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            session_id,
            operation: "poll_workflow_dev".into(),
        },
    )
    .await;
    Ok(no_store(Json(result)))
}

#[utoipa::path(post,path="/projects/{project_id}/environments/{environment_id}/workflows/dev/{session_id}/requests/{request_id}",tag="Workflows",params(("project_id"=i32,Path),("environment_id"=i32,Path),("session_id"=Uuid,Path),("request_id"=Uuid,Path)),request_body=DevReply,
responses((status=204,description="Development tunnel operation completed"),
(status=400,description="Invalid request"),(status=401,description="Unauthorized"),(status=403,description="Requires workflow write and payload access"),(status=409,description="Session unavailable"),(status=413,description="Message too large"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn respond_workflow_dev(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, session_id, request_id)): Path<(i32, i32, Uuid, Uuid)>,
    body: Body,
) -> Result<Response, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    project_permission_guard!(
        auth,
        WorkflowPayloadsRead,
        project_id,
        &state.project_access_checker
    );
    let bytes = axum::body::to_bytes(body, crate::dev::DEV_BODY_LIMIT)
        .await
        .map_err(|_| {
            temps_core::problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                .with_title("Development tunnel message too large")
                .with_detail("Development tunnel messages are limited to 8 MiB")
        })?;
    let request: DevReply = serde_json::from_slice(&bytes).map_err(|_| {
        temps_core::problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid development message")
            .with_detail("Expected the documented JSON request shape")
    })?;
    state
        .service
        .respond_dev(project_id, environment_id, session_id, request_id, request)
        .await?;

    record_audit(
        &state,
        &WorkflowDevAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            session_id,
            operation: "respond_workflow_dev".into(),
        },
    )
    .await;
    Ok(no_store(StatusCode::NO_CONTENT))
}
