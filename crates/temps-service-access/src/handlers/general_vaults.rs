// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{handler::audit, AccessAppState};
use crate::{types::AccessPageQuery, vault_types::*};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::sync::Arc;
use temps_authz::{deny_deployment_token, permission_guard, RequireAuth};
use temps_core::{problemdetails::Problem, RequestMetadata};

#[utoipa::path(get,path="/access-vaults",tag="Vault",params(AccessPageQuery),
responses((status=200,description="Success",body=AccessVaultList), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn list_access_vaults_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Query(query): Query<AccessPageQuery>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsRead);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    let project_id = None;
    let response = state.service.list_vaults(project_id, query).await?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(get,path="/access-vaults/{vault_id}",tag="Vault",params(("vault_id" = String, Path), VaultConsumerQuery),
responses((status=200,description="Success",body=AccessVaultDetail), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn get_access_vault_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(vault_id): Path<String>,
    Query(consumer): Query<VaultConsumerQuery>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsRead);
    can_read_vault(&state, &auth, &vault_id, &consumer).await?;
    let project_id = None;
    let response = state.service.get_vault(project_id, &vault_id).await?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(post,path="/access-vaults",tag="Vault",
request_body=SaveAccessVault,
responses((status=201,description="Success",body=AccessVaultResponse), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn create_access_vault_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessVault>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    let project_id = None;
    let response = state.service.save_vault(project_id, None, request).await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "CREATE_ACCESS_VAULT",
    )
    .await;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(put,path="/access-vaults/{vault_id}",tag="Vault",params(("vault_id" = String, Path)),
request_body=SaveAccessVault,
responses((status=200,description="Success",body=AccessVaultResponse), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn update_access_vault_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(vault_id): Path<String>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessVault>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    let project_id = None;
    let response = state
        .service
        .save_vault(project_id, Some(vault_id), request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "UPDATE_ACCESS_VAULT",
    )
    .await;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(get,path="/access-vaults/{vault_id}/bundles",tag="Vault",params(("vault_id" = String, Path), AccessPageQuery, VaultConsumerQuery),
responses((status=200,description="Success",body=AccessBundleList), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn list_access_bundles_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(vault_id): Path<String>,
    Query(consumer): Query<VaultConsumerQuery>,
    Query(query): Query<AccessPageQuery>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsRead);
    can_read_vault(&state, &auth, &vault_id, &consumer).await?;
    let project_id = None;
    let response = state
        .service
        .list_bundles(project_id, &vault_id, query)
        .await?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(post,path="/access-vaults/{vault_id}/bundles",tag="Vault",params(("vault_id" = String, Path)),
request_body=SaveAccessBundle,
responses((status=201,description="Success",body=AccessBundleResponse), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn create_access_bundle_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(vault_id): Path<String>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessBundle>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    let project_id = None;
    let response = state
        .service
        .save_bundle(project_id, &vault_id, None, request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        (&response.id, &vault_id),
        response.version,
        "CREATE_ACCESS_BUNDLE",
    )
    .await;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(put,path="/access-vaults/{vault_id}/bundles/{bundle_id}",tag="Vault",params(("vault_id" = String, Path), ("bundle_id" = String, Path)),
request_body=SaveAccessBundle,
responses((status=200,description="Success",body=AccessBundleResponse), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn update_access_bundle_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((vault_id, bundle_id)): Path<(String, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessBundle>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    let project_id = None;
    let response = state
        .service
        .save_bundle(project_id, &vault_id, Some(bundle_id), request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        (&response.id, &vault_id),
        response.version,
        "UPDATE_ACCESS_BUNDLE",
    )
    .await;
    Ok((StatusCode::OK, Json(response)))
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
pub struct VaultConsumerQuery {
    pub sandbox_id: Option<String>,
}

async fn can_read_vault(
    state: &AccessAppState,
    auth: &temps_authz::AuthContext,
    vault_id: &str,
    consumer: &VaultConsumerQuery,
) -> Result<(), Problem> {
    if auth.is_admin() {
        return Ok(());
    }
    let sandbox_id = consumer.sandbox_id.as_deref().ok_or_else(|| {
        temps_core::error_builder::forbidden()
            .detail("Select an owned sandbox to inspect available vault policies")
            .build()
    })?;
    let link = state
        .service
        .session_eligibility(vault_id, sandbox_id, auth.user_id(), false)
        .await?;
    if let Some(project_id) = link.project_id {
        temps_authz::project_permission_guard!(
            auth,
            ProjectsRead,
            project_id,
            &state.project_access_checker
        );
    }
    Ok(())
}
