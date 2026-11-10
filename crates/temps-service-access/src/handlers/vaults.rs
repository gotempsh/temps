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
use temps_authz::{
    deny_deployment_token, project_permission_guard, project_scope_guard, RequireAuth,
};
use temps_core::{problemdetails::Problem, RequestMetadata};

#[utoipa::path(get,path="/projects/{project_id}/access-vaults",tag="Vault",params(("project_id" = i32, Path), AccessPageQuery),
responses((status=200,description="Success",body=AccessVaultList), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn list_access_vaults(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(project_id): Path<i32>,
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
    let response = state.service.list_vaults(project_id, query).await?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(get,path="/projects/{project_id}/access-vaults/{vault_id}",tag="Vault",params(("project_id" = i32, Path), ("vault_id" = String, Path)),
responses((status=200,description="Success",body=AccessVaultDetail), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn get_access_vault(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, vault_id)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsRead,
        project_id,
        &state.project_access_checker
    );
    let response = state.service.get_vault(project_id, &vault_id).await?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(post,path="/projects/{project_id}/access-vaults",tag="Vault",params(("project_id" = i32, Path)),
request_body=SaveAccessVault,
responses((status=201,description="Success",body=AccessVaultResponse), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn create_access_vault(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessVault>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        &state.project_access_checker
    );
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

#[utoipa::path(put,path="/projects/{project_id}/access-vaults/{vault_id}",tag="Vault",params(("project_id" = i32, Path), ("vault_id" = String, Path)),
request_body=SaveAccessVault,
responses((status=200,description="Success",body=AccessVaultResponse), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn update_access_vault(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, vault_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessVault>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        &state.project_access_checker
    );
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

#[utoipa::path(get,path="/projects/{project_id}/access-vaults/{vault_id}/bundles",tag="Vault",params(("project_id" = i32, Path), ("vault_id" = String, Path), AccessPageQuery),
responses((status=200,description="Success",body=AccessBundleList), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn list_access_bundles(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, vault_id)): Path<(i32, String)>,
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
    let response = state
        .service
        .list_bundles(project_id, &vault_id, query)
        .await?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(post,path="/projects/{project_id}/access-vaults/{vault_id}/bundles",tag="Vault",params(("project_id" = i32, Path), ("vault_id" = String, Path)),
request_body=SaveAccessBundle,
responses((status=201,description="Success",body=AccessBundleResponse), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn create_access_bundle(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, vault_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessBundle>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        &state.project_access_checker
    );
    let response = state
        .service
        .save_bundle(project_id, &vault_id, None, request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "CREATE_ACCESS_BUNDLE",
    )
    .await;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(put,path="/projects/{project_id}/access-vaults/{vault_id}/bundles/{bundle_id}",tag="Vault",params(("project_id" = i32, Path), ("vault_id" = String, Path), ("bundle_id" = String, Path)),
request_body=SaveAccessBundle,
responses((status=200,description="Success",body=AccessBundleResponse), (status=400,description="Invalid selection"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Resource not found"),(status=409,description="Version conflict"),(status=500,description="Storage failure")),security(("bearer_auth"=[])))]
pub async fn update_access_bundle(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, vault_id, bundle_id)): Path<(i32, String, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessBundle>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    if !auth.is_admin() {
        return Err(temps_core::error_builder::forbidden()
            .detail("Vault management requires an instance administrator")
            .build());
    }
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        &state.project_access_checker
    );
    let response = state
        .service
        .save_bundle(project_id, &vault_id, Some(bundle_id), request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "UPDATE_ACCESS_BUNDLE",
    )
    .await;
    Ok((StatusCode::OK, Json(response)))
}
