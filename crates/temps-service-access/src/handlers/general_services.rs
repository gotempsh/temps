// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{handler::audit, AccessAppState};
use crate::{broker::AccessCapability, types::*};
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
            .detail("Service management requires an instance administrator")
            .build())
    }
}
#[derive(serde::Deserialize, utoipa::IntoParams)]
pub struct CredentialSourceQuery {
    pub source_project_id: Option<i32>,
}
#[utoipa::path(get,path="/service-access/capability",tag="Vault", responses((status=200,body=AccessCapability),(status=401),(status=403)), security(("bearer_auth"=[])))]
pub async fn general_access_capability(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
) -> Result<Json<AccessCapability>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, SandboxesRead);
    let mut result = state.service.capability();
    result.can_manage_services = auth.is_admin()
        && auth.has_permission(&temps_authz::permissions::Permission::ProjectsWrite);
    result.can_manage_requests =
        auth.has_permission(&temps_authz::permissions::Permission::SandboxesWrite);
    result.can_use_project_github = result.can_manage_services
        && auth.has_permission(&temps_authz::permissions::Permission::GitConnectionsRead);
    result.setup_path = Some("/vault".into());
    Ok(Json(result))
}
#[utoipa::path(get,path="/access-services",tag="Vault",params(AccessPageQuery), responses((status=200,body=AccessServiceList),(status=401),(status=403),(status=500)), security(("bearer_auth"=[])))]
pub async fn list_access_services_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Query(query): Query<AccessPageQuery>,
) -> Result<Json<AccessServiceList>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsRead);
    manager(&auth)?;
    Ok(Json(state.service.list_services(None, query).await?))
}
#[utoipa::path(get,path="/access-services/{service_id}",tag="Vault",params(("service_id"=String,Path)), responses((status=200,body=AccessServiceResponse),(status=401),(status=403),(status=404),(status=500)), security(("bearer_auth"=[])))]
pub async fn get_access_service_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(id): Path<String>,
) -> Result<Json<AccessServiceResponse>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsRead);
    manager(&auth)?;
    Ok(Json(state.service.get_service(None, &id).await?))
}
#[utoipa::path(post,path="/access-services",tag="Vault",params(CredentialSourceQuery),request_body=SaveAccessService, responses((status=201,body=AccessServiceResponse),(status=400),(status=401),(status=403),(status=404),(status=409),(status=500)), security(("bearer_auth"=[])))]
pub async fn create_access_service_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Query(query): Query<CredentialSourceQuery>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessService>,
) -> Result<(StatusCode, Json<AccessServiceResponse>), Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    manager(&auth)?;
    if request.policy.github_repository.is_some() {
        permission_guard!(auth, GitConnectionsRead);
        permission_guard!(auth, GitRepositoriesRead);
    }
    let row = state
        .service
        .save_service(query.source_project_id, None, request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        row.project_id,
        super::audit::AuditResource::service(&row.id, row.version),
        row.version,
        "CREATE_ACCESS_SERVICE",
    )
    .await;
    Ok((StatusCode::CREATED, Json(row)))
}
#[utoipa::path(put,path="/access-services/{service_id}",tag="Vault",params(("service_id"=String,Path)),request_body=SaveAccessService, responses((status=200,body=AccessServiceResponse),(status=400),(status=401),(status=403),(status=404),(status=409),(status=500)), security(("bearer_auth"=[])))]
pub async fn update_access_service_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(id): Path<String>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessService>,
) -> Result<Json<AccessServiceResponse>, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    manager(&auth)?;
    if request.policy.github_repository.is_some() {
        permission_guard!(auth, GitConnectionsRead);
        permission_guard!(auth, GitRepositoriesRead);
    }
    let existing = state.service.get_service(None, &id).await?;
    // A migrated bearer's historical project is provenance, not its owner.
    // GitHub credentials still validate the project owning their connection.
    let source_project_id = request
        .policy
        .github_repository
        .as_ref()
        .and(existing.project_id);
    let row = state
        .service
        .save_service(source_project_id, Some(id), request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        row.project_id,
        super::audit::AuditResource::service(&row.id, row.version),
        row.version,
        "UPDATE_ACCESS_SERVICE",
    )
    .await;
    Ok(Json(row))
}

#[utoipa::path(delete,path="/access-services/{service_id}",tag="Vault",params(("service_id"=String,Path)),responses((status=204),(status=401),(status=403),(status=404),(status=500)),security(("bearer_auth"=[])))]
pub async fn delete_access_service_general(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(service_id): Path<String>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    deny_deployment_token!(auth);
    permission_guard!(auth, ProjectsWrite);
    manager(&auth)?;
    let version = state.service.retire_service(&service_id).await?;
    audit(
        &state,
        &auth,
        metadata,
        None,
        super::audit::AuditResource::service(&service_id, version),
        version,
        "DELETE_ACCESS_SERVICE",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
