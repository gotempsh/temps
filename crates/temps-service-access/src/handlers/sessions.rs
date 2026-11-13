// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{handler::audit, AccessAppState};
use crate::session_types::*;
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

#[utoipa::path(get,path="/projects/{project_id}/access-sessions",tag="Vault sessions",params(("project_id" = i32, Path), AccessSessionQuery),
responses((status=200,description="Session authorizations",body=AccessSessionList),
(status=400,description="Invalid session"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),
(status=404,description="Scoped resource not found"),(status=409,description="Policy changed"),(status=500,description="Session storage failed"),(status=503,description="Worker unavailable")),security(("bearer_auth"=[])))]
pub async fn list_access_sessions(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<AccessSessionQuery>,
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
    Ok(Json(
        state
            .service
            .list_sessions(project_id, auth.user_id(), auth.is_admin(), query)
            .await?,
    ))
}

#[utoipa::path(get,path="/projects/{project_id}/access-sessions/{session_id}",tag="Vault sessions",params(("project_id" = i32, Path), ("session_id" = String, Path)),
responses((status=200,description="Session policies and worker receipts",body=AccessSessionDetail),
(status=400,description="Invalid session"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),
(status=404,description="Scoped resource not found"),(status=409,description="Policy changed"),(status=500,description="Session storage failed"),(status=503,description="Worker unavailable")),security(("bearer_auth"=[])))]
pub async fn get_access_session(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, session_id)): Path<(i32, String)>,
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
    Ok(Json(
        state
            .service
            .get_session(project_id, &session_id, auth.user_id(), auth.is_admin())
            .await?,
    ))
}

#[utoipa::path(post,path="/projects/{project_id}/access-vaults/{vault_id}/sessions",tag="Vault sessions",params(("project_id" = i32, Path), ("vault_id" = String, Path)),
request_body=CreateAccessSession,
responses((status=202,description="Session issued; worker enforcement is asynchronous",body=AccessSessionResponse),
(status=400,description="Invalid session"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),
(status=404,description="Scoped resource not found"),(status=409,description="Policy changed"),(status=500,description="Session storage failed"),(status=503,description="Worker unavailable")),security(("bearer_auth"=[])))]
pub async fn create_access_session(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, vault_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    Json(request): Json<CreateAccessSession>,
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
    temps_core::ai_tool_call::require_human_caller(
        ai_call.as_ref().map(|m| &m.0),
        "create_access_session",
    )?;
    crate::service::sessions::validate(&request)?;
    let vault = state.service.get_vault(project_id, &vault_id).await?;
    // Every selected policy must be present at the exact reviewed version.
    // Session issuance rechecks these versions while holding the service locks.
    for selected in &request.services {
        if !vault
            .services
            .iter()
            .any(|s| s.id == selected.service_id && s.version == selected.version)
        {
            return Err(crate::error::AccessError::Conflict {
                id: selected.service_id.clone(),
                version: selected.version,
            }
            .into());
        }
    }
    if vault.services.iter().any(|s| {
        s.policy.github_repository.is_some()
            && request.services.iter().any(|v| v.service_id == s.id)
    }) {
        project_permission_guard!(
            auth,
            GitRepositoriesRead,
            project_id,
            &state.project_access_checker
        );
        temps_authz::permission_guard!(auth, GitConnectionsRead);
    }
    let response = state
        .coordinator
        .connect_session(
            project_id,
            &vault_id,
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
        (&response.id, &response.vault_id),
        1,
        "CREATE_ACCESS_SESSION",
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[utoipa::path(post,path="/projects/{project_id}/access-sessions/{session_id}/revoke",tag="Vault sessions",params(("project_id" = i32, Path), ("session_id" = String, Path)),
responses((status=202,description="Revocation requested; inspect worker receipts",body=AccessSessionResponse),
(status=400,description="Invalid session"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),
(status=404,description="Scoped resource not found"),(status=409,description="Policy changed"),(status=500,description="Session storage failed"),(status=503,description="Worker unavailable")),security(("bearer_auth"=[])))]
pub async fn revoke_access_session(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, session_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
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
        .revoke_session(project_id, &session_id, auth.user_id(), auth.is_admin())
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        (&response.id, &response.vault_id),
        1,
        "REVOKE_ACCESS_SESSION",
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(response)))
}
