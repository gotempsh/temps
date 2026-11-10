// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{handler::audit, AccessAppState};
use crate::{link_types::*, types::AccessPageQuery};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use std::sync::Arc;
use temps_authz::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::{problemdetails::Problem, RequestMetadata};

fn manager(auth: &temps_authz::AuthContext) -> Result<(), Problem> {
    if auth.is_admin() {
        Ok(())
    } else {
        Err(temps_core::error_builder::forbidden()
            .detail("Vault links require an instance administrator")
            .build())
    }
}

#[utoipa::path(get, path="/access-vaults/{vault_id}/links", tag="Vault", params(("vault_id"=String, Path), AccessPageQuery), responses((status=200,body=VaultLinkList),(status=401),(status=403),(status=404),(status=500)), security(("bearer_auth"=[])))]
pub async fn list_vault_links(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(vault_id): Path<String>,
    Query(query): Query<AccessPageQuery>,
) -> Result<Json<VaultLinkList>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsRead);
    manager(&auth)?;
    Ok(Json(
        state.service.list_vault_links(&vault_id, query).await?,
    ))
}
#[utoipa::path(post, path="/access-vaults/{vault_id}/links", tag="Vault", params(("vault_id"=String, Path)), request_body=CreateVaultLink, responses((status=201,body=VaultLinkResponse),(status=400),(status=401),(status=403),(status=404),(status=409),(status=500)), security(("bearer_auth"=[])))]
pub async fn create_vault_link(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(vault_id): Path<String>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    confirmation: Option<Extension<temps_core::ai_tool_call::ConfirmedAiWrite>>,
    Json(request): Json<CreateVaultLink>,
) -> Result<(StatusCode, Json<VaultLinkResponse>), Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    manager(&auth)?;
    temps_core::ai_tool_call::require_human_confirmation(
        ai_call.as_ref().map(|m| &m.0),
        confirmation.as_ref().map(|m| &m.0),
        "create_vault_link",
    )?;
    let row = state.service.link_consumer(&vault_id, request).await?;
    audit(
        &state,
        &auth,
        metadata,
        row.project_id,
        &row.id,
        1,
        "CREATE_ACCESS_VAULT_LINK",
    )
    .await;
    Ok((StatusCode::CREATED, Json(row.into())))
}
#[utoipa::path(delete, path="/access-vaults/{vault_id}/links/{link_id}", tag="Vault", params(("vault_id"=String, Path),("link_id"=String, Path)), responses((status=204),(status=401),(status=403),(status=404),(status=500)), security(("bearer_auth"=[])))]
pub async fn delete_vault_link(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((vault_id, link_id)): Path<(String, String)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    manager(&auth)?;
    state.service.unlink_vault(&vault_id, &link_id).await?;
    audit(
        &state,
        &auth,
        metadata,
        None,
        &link_id,
        1,
        "REVOKE_ACCESS_VAULT_LINK",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get,path="/sandboxes/{sandbox_id}/access-vaults",tag="Vault",params(("sandbox_id"=String,Path),AccessPageQuery),responses((status=200,body=crate::vault_types::AccessVaultList),(status=401),(status=403),(status=404),(status=500)),security(("bearer_auth"=[])))]
pub async fn sandbox_access_vaults(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(sandbox_id): Path<String>,
    Query(query): Query<AccessPageQuery>,
) -> Result<Json<crate::vault_types::AccessVaultList>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, SandboxesRead);
    let (id, project_id) = state
        .service
        .owned_consumer(&sandbox_id, auth.user_id(), auth.is_admin())
        .await?;
    let allowed_project = if let Some(project_id) = project_id {
        let access = async {
            temps_authz::project_permission_guard!(
                auth,
                ProjectsRead,
                project_id,
                &state.project_access_checker
            );
            Ok::<(), Problem>(())
        }
        .await;
        match access {
            Ok(()) => Some(project_id),
            Err(e) if e.status_code == StatusCode::FORBIDDEN => None,
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    Ok(Json(
        state
            .service
            .list_consumer_vaults(allowed_project, Some(id), query)
            .await?,
    ))
}
#[utoipa::path(get,path="/projects/{project_id}/linked-vaults",tag="Vault",params(("project_id"=i32,Path),AccessPageQuery),responses((status=200,body=crate::vault_types::AccessVaultList),(status=401),(status=403),(status=404),(status=500)),security(("bearer_auth"=[])))]
pub async fn project_access_vaults(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<AccessPageQuery>,
) -> Result<Json<crate::vault_types::AccessVaultList>, Problem> {
    temps_authz::project_scope_guard!(auth, project_id);
    temps_authz::project_permission_guard!(
        auth,
        ProjectsRead,
        project_id,
        &state.project_access_checker
    );
    Ok(Json(
        state
            .service
            .list_consumer_vaults(Some(project_id), None, query)
            .await?,
    ))
}

#[utoipa::path(delete,path="/access-vaults/{vault_id}",tag="Vault",params(("vault_id"=String,Path)),responses((status=204),(status=401),(status=403),(status=404),(status=500)),security(("bearer_auth"=[])))]
pub async fn delete_access_vault_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(vault_id): Path<String>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    manager(&auth)?;
    state.service.retire_vault_resource(&vault_id, None).await?;
    audit(
        &state,
        &auth,
        metadata,
        None,
        &vault_id,
        1,
        "DELETE_ACCESS_VAULT_GENERAL",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete,path="/access-vaults/{vault_id}/bundles/{bundle_id}",tag="Vault",params(("vault_id"=String,Path), ("bundle_id"=String,Path)),responses((status=204),(status=401),(status=403),(status=404),(status=500)),security(("bearer_auth"=[])))]
pub async fn delete_access_bundle_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((vault_id, bundle_id)): Path<(String, String)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    manager(&auth)?;
    state
        .service
        .retire_vault_resource(&vault_id, Some(bundle_id.as_str()))
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        None,
        &bundle_id,
        1,
        "DELETE_ACCESS_BUNDLE_GENERAL",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get,path="/access-vaults/{vault_id}/consumers/{sandbox_id}",tag="Vault",params(("vault_id"=String,Path),("sandbox_id"=String,Path)),responses((status=200,body=VaultLinkResponse),(status=401),(status=403),(status=404),(status=500)),security(("bearer_auth"=[])))]
pub async fn get_vault_eligibility(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((vault_id, sandbox_id)): Path<(String, String)>,
) -> Result<Json<VaultLinkResponse>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, SandboxesRead);
    let link = state
        .service
        .session_eligibility(&vault_id, &sandbox_id, auth.user_id(), auth.is_admin())
        .await?;
    if let Some(project_id) = link.project_id {
        temps_authz::project_permission_guard!(
            auth,
            ProjectsRead,
            project_id,
            &state.project_access_checker
        );
    }
    Ok(Json(link))
}
