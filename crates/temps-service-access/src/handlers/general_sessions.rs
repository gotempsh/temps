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
use temps_authz::{deny_deployment_token, permission_guard, project_permission_guard, RequireAuth};
use temps_core::{problemdetails::Problem, RequestMetadata};

#[utoipa::path(get,path="/access-sessions",tag="Vault sessions",params(AccessSessionQuery),
responses((status=200,description="Session authorizations",body=AccessSessionList),
(status=400,description="Invalid session"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),
(status=404,description="Scoped resource not found"),(status=409,description="Policy changed"),(status=500,description="Session storage failed"),(status=503,description="Worker unavailable")),security(("bearer_auth"=[])))]
pub async fn list_access_sessions_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Query(query): Query<AccessSessionQuery>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, SandboxesRead);
    let mut direct_only = false;
    if !auth.is_admin() {
        let sandbox_id = query.sandbox_id.as_deref().ok_or_else(|| {
            temps_core::error_builder::bad_request()
                .detail("Select an owned sandbox to list vault sessions")
                .build()
        })?;
        let sandbox = state
            .service
            .owned_consumer(sandbox_id, auth.user_id(), false)
            .await?;
        if let Some(project_id) = sandbox.1 {
            match session_project_read(&auth, &state, project_id).await {
                Ok(()) => {}
                Err(problem) if problem.status_code == StatusCode::FORBIDDEN => direct_only = true,
                Err(problem) => return Err(problem),
            }
        }
    }
    Ok(Json(
        state
            .service
            .list_sessions_filtered(None, auth.user_id(), auth.is_admin(), query, direct_only)
            .await?,
    ))
}

#[utoipa::path(get,path="/access-sessions/{session_id}",tag="Vault sessions",params(("session_id" = String, Path)),
responses((status=200,description="Session policies and worker receipts",body=AccessSessionDetail),
(status=400,description="Invalid session"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),
(status=404,description="Scoped resource not found"),(status=409,description="Policy changed"),(status=500,description="Session storage failed"),(status=503,description="Worker unavailable")),security(("bearer_auth"=[])))]
pub async fn get_access_session_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, SandboxesRead);
    if let Some(project_id) = state
        .service
        .session_project_scope(&session_id, auth.user_id(), auth.is_admin())
        .await?
    {
        session_project_read(&auth, &state, project_id).await?;
    }
    let project_id = None;
    Ok(Json(
        state
            .service
            .get_session(project_id, &session_id, auth.user_id(), auth.is_admin())
            .await?,
    ))
}

#[utoipa::path(post,path="/access-vaults/{vault_id}/sessions",tag="Vault sessions",params(("vault_id" = String, Path)),
request_body=CreateAccessSession,
responses((status=202,description="Session issued; worker enforcement is asynchronous",body=AccessSessionResponse),
(status=400,description="Invalid session"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),
(status=404,description="Scoped resource not found"),(status=409,description="Policy changed"),(status=500,description="Session storage failed"),(status=503,description="Worker unavailable")),security(("bearer_auth"=[])))]
pub async fn create_access_session_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(vault_id): Path<String>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    confirmation: Option<Extension<temps_core::ai_tool_call::ConfirmedAiWrite>>,
    Json(mut request): Json<CreateAccessSession>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, SandboxesWrite);
    let project_id = None;
    temps_core::ai_tool_call::require_human_confirmation(
        ai_call.as_ref().map(|m| &m.0),
        confirmation.as_ref().map(|m| &m.0),
        "create_access_session_general",
    )?;
    crate::service::sessions::validate(&request)?;
    let link = state
        .service
        .session_eligibility(
            &vault_id,
            request.consumer.sandbox_id(),
            auth.user_id(),
            auth.is_admin(),
        )
        .await?;
    if let Some(project_id) = link.project_id {
        project_permission_guard!(
            auth,
            ProjectsWrite,
            project_id,
            &state.project_access_checker
        );
    }
    if let Some(project_id) = link.project_id {
        project_permission_guard!(
            auth,
            SandboxesWrite,
            project_id,
            &state.project_access_checker
        );
    }
    request.authorized_link_id = Some(link.id);
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
        for service in vault.services.iter().filter(|s| {
            s.policy.github_repository.is_some()
                && request.services.iter().any(|v| v.service_id == s.id)
        }) {
            let source_project = service.project_id.ok_or_else(|| {
                temps_core::error_builder::forbidden()
                    .detail("GitHub service has no credential source project")
                    .build()
            })?;
            // GitHub grants retain the source project's worker authorization,
            // including when coverage comes from a direct sandbox link.
            project_permission_guard!(
                auth,
                ProjectsWrite,
                source_project,
                &state.project_access_checker
            );
            project_permission_guard!(
                auth,
                SandboxesWrite,
                source_project,
                &state.project_access_checker
            );
            project_permission_guard!(
                auth,
                GitRepositoriesRead,
                source_project,
                &state.project_access_checker
            );
        }
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
        &response.id,
        1,
        "CREATE_ACCESS_SESSION",
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[utoipa::path(post,path="/access-sessions/{session_id}/revoke",tag="Vault sessions",params(("session_id" = String, Path)),
responses((status=202,description="Revocation requested; inspect worker receipts",body=AccessSessionResponse),
(status=400,description="Invalid session"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),
(status=404,description="Scoped resource not found"),(status=409,description="Policy changed"),(status=500,description="Session storage failed"),(status=503,description="Worker unavailable")),security(("bearer_auth"=[])))]
pub async fn revoke_access_session_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(session_id): Path<String>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, SandboxesWrite);
    if let Some(project_id) = state
        .service
        .session_project_scope(&session_id, auth.user_id(), auth.is_admin())
        .await?
    {
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
    }
    let project_id = None;
    let response = state
        .service
        .revoke_session(project_id, &session_id, auth.user_id(), auth.is_admin())
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        1,
        "REVOKE_ACCESS_SESSION",
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

async fn session_project_read(
    auth: &temps_authz::AuthContext,
    state: &AccessAppState,
    project_id: i32,
) -> Result<(), Problem> {
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
    Ok(())
}
