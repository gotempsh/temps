// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{approval::approve_service_grant, audit::AccessAudit, AccessAppState};
use crate::assignment_types::*;
use crate::session_types::*;
use crate::vault_types::*;
use crate::{
    broker::{AccessCapability, AccessCapabilityState},
    error::AccessError,
    types::*,
};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use temps_authz::{
    deny_deployment_token, permission_guard, project_permission_guard, project_scope_guard,
    RequireAuth,
};
use temps_core::{
    problemdetails::{self, Problem},
    AuditContext, RequestMetadata,
};
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(paths(super::links::get_vault_eligibility,super::general_services::delete_access_service_general,super::links::delete_access_vault_general,super::links::delete_access_bundle_general,super::links::sandbox_access_vaults,super::links::project_access_vaults,super::general_services::general_access_capability,super::general_services::list_access_services_general,super::general_services::get_access_service_general,super::general_services::create_access_service_general,super::general_services::update_access_service_general,super::general_sessions::list_access_sessions_general,super::general_sessions::get_access_session_general,super::general_sessions::create_access_session_general,super::general_sessions::revoke_access_session_general,super::links::list_vault_links,super::links::create_vault_link,super::links::delete_vault_link,super::general_vaults::list_access_vaults_general,super::general_vaults::get_access_vault_general,super::general_vaults::create_access_vault_general,super::general_vaults::update_access_vault_general,super::general_vaults::list_access_bundles_general,super::general_vaults::create_access_bundle_general,super::general_vaults::update_access_bundle_general,super::assignments::list_sandbox_services, super::assignments::request_sandbox_service, super::assignments::approve_sandbox_service, super::assignments::revoke_sandbox_service,super::sessions::list_access_sessions, super::sessions::get_access_session, super::sessions::create_access_session, super::sessions::revoke_access_session,super::vaults::list_access_vaults,super::vaults::get_access_vault,super::vaults::create_access_vault,super::vaults::update_access_vault,super::vaults::list_access_bundles,super::vaults::create_access_bundle,super::vaults::update_access_bundle,super::approval::approve_service_grant, access_capability, get_access_service, list_access_services, create_access_service, update_access_service, list_service_grants, request_service_grant, revoke_service_grant),
    components(schemas(crate::link_types::CreateVaultLink,crate::link_types::VaultLinkResponse,crate::link_types::VaultLinkList,RequestSandboxService, ApproveSandboxService, RevokeSandboxService, SandboxServiceResponse, SandboxServiceList, SandboxServiceStatus,CreateAccessSession, AccessSessionConsumer, SessionServiceVersion, AccessSessionResponse, AccessSessionStatus, AccessSessionDetail, AccessSessionList,AccessVaultResponse, AccessVaultOverview, AccessVaultList, AccessVaultDetail, SaveAccessVault, AccessBundleResponse, AccessBundleList, SaveAccessBundle,ApproveServiceGrant, ServiceGrantApproval, AccessCapability, AccessCapabilityState, SandboxAccessPolicy, AccessMethod, SaveAccessService, AccessServiceResponse, AccessServiceList, RequestServiceGrant, RevokeServiceGrant, ServiceGrantResponse, ServiceGrantList, ServiceGrantStatus)),
    tags((name="Sandbox service access", description="Saved service policies and time-limited access requests; runtime integration is not installed")))]
pub struct ServiceAccessApiDoc;

pub fn configure_routes() -> Router<Arc<AccessAppState>> {
    Router::new()
        .route(
            "/access-vaults/{vault_id}/consumers/{sandbox_id}",
            get(super::links::get_vault_eligibility),
        )
        .route(
            "/sandboxes/{sandbox_id}/access-vaults",
            get(super::links::sandbox_access_vaults),
        )
        .route(
            "/projects/{project_id}/linked-vaults",
            get(super::links::project_access_vaults),
        )
        .route(
            "/service-access/capability",
            get(super::general_services::general_access_capability),
        )
        .route(
            "/access-services",
            get(super::general_services::list_access_services_general)
                .post(super::general_services::create_access_service_general),
        )
        .route(
            "/access-services/{service_id}",
            get(super::general_services::get_access_service_general)
                .put(super::general_services::update_access_service_general)
                .delete(super::general_services::delete_access_service_general),
        )
        .route(
            "/access-sessions",
            get(super::general_sessions::list_access_sessions_general),
        )
        .route(
            "/access-sessions/{session_id}",
            get(super::general_sessions::get_access_session_general),
        )
        .route(
            "/access-sessions/{session_id}/revoke",
            post(super::general_sessions::revoke_access_session_general),
        )
        .route(
            "/access-vaults/{vault_id}/sessions",
            post(super::general_sessions::create_access_session_general),
        )
        .route(
            "/access-vaults/{vault_id}/links",
            get(super::links::list_vault_links).post(super::links::create_vault_link),
        )
        .route(
            "/access-vaults/{vault_id}/links/{link_id}",
            axum::routing::delete(super::links::delete_vault_link),
        )
        .route(
            "/access-vaults",
            get(super::general_vaults::list_access_vaults_general)
                .post(super::general_vaults::create_access_vault_general),
        )
        .route(
            "/access-vaults/{vault_id}",
            get(super::general_vaults::get_access_vault_general)
                .put(super::general_vaults::update_access_vault_general)
                .delete(super::links::delete_access_vault_general),
        )
        .route(
            "/access-vaults/{vault_id}/bundles",
            get(super::general_vaults::list_access_bundles_general)
                .post(super::general_vaults::create_access_bundle_general),
        )
        .route(
            "/access-vaults/{vault_id}/bundles/{bundle_id}",
            axum::routing::put(super::general_vaults::update_access_bundle_general)
                .delete(super::links::delete_access_bundle_general),
        )
        .route(
            "/projects/{project_id}/sandboxes/{sandbox_id}/services",
            get(super::assignments::list_sandbox_services)
                .post(super::assignments::request_sandbox_service),
        )
        .route(
            "/projects/{project_id}/sandboxes/{sandbox_id}/services/{access_id}/approve",
            post(super::assignments::approve_sandbox_service),
        )
        .route(
            "/projects/{project_id}/sandboxes/{sandbox_id}/services/{access_id}/revoke",
            post(super::assignments::revoke_sandbox_service),
        )
        .route(
            "/projects/{project_id}/access-sessions",
            get(super::sessions::list_access_sessions),
        )
        .route(
            "/projects/{project_id}/access-sessions/{session_id}",
            get(super::sessions::get_access_session),
        )
        .route(
            "/projects/{project_id}/access-sessions/{session_id}/revoke",
            post(super::sessions::revoke_access_session),
        )
        .route(
            "/projects/{project_id}/access-vaults/{vault_id}/sessions",
            post(super::sessions::create_access_session),
        )
        .route(
            "/projects/{project_id}/access-vaults",
            get(super::vaults::list_access_vaults).post(super::vaults::create_access_vault),
        )
        .route(
            "/projects/{project_id}/access-vaults/{vault_id}",
            get(super::vaults::get_access_vault).put(super::vaults::update_access_vault),
        )
        .route(
            "/projects/{project_id}/access-vaults/{vault_id}/bundles",
            get(super::vaults::list_access_bundles).post(super::vaults::create_access_bundle),
        )
        .route(
            "/projects/{project_id}/access-vaults/{vault_id}/bundles/{bundle_id}",
            axum::routing::put(super::vaults::update_access_bundle),
        )
        .route(
            "/projects/{project_id}/sandboxes/{sandbox_id}/service-grants/approve",
            post(approve_service_grant),
        )
        .route(
            "/projects/{project_id}/service-access/capability",
            get(access_capability),
        )
        .route(
            "/projects/{project_id}/access-services",
            get(list_access_services).post(create_access_service),
        )
        .route(
            "/projects/{project_id}/access-services/{service_id}",
            get(get_access_service).put(update_access_service),
        )
        .route(
            "/projects/{project_id}/sandboxes/{sandbox_id}/service-grants",
            get(list_service_grants).post(request_service_grant),
        )
        .route(
            "/projects/{project_id}/sandboxes/{sandbox_id}/service-grants/{grant_id}/revoke",
            post(revoke_service_grant),
        )
}

impl From<AccessError> for Problem {
    fn from(error: AccessError) -> Self {
        let (status, title) = match &error {
            AccessError::VaultSessionRequired { .. } => (
                StatusCode::CONFLICT,
                "Vault service requires a bundle session",
            ),
            AccessError::UncoveredSandbox { .. } => {
                (StatusCode::FORBIDDEN, "Sandbox is not linked to vault")
            }
            AccessError::GitLinkConflict { .. } => (
                StatusCode::CONFLICT,
                "GitHub credential source restricts this link",
            ),
            AccessError::NotFound { .. } => {
                (StatusCode::NOT_FOUND, "Service access resource not found")
            }
            AccessError::Validation { .. } => {
                (StatusCode::BAD_REQUEST, "Invalid service access request")
            }
            AccessError::Conflict { .. } => {
                (StatusCode::CONFLICT, "Service access version changed")
            }
            AccessError::Delivery { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Service access delivery could not be queued",
            ),
            AccessError::ResourceDatabase { .. } | AccessError::Database { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Service access storage failed",
            ),
            AccessError::Encryption { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Service credential encryption failed",
            ),
            AccessError::StoredPolicy { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Stored service access policy is invalid",
            ),
            AccessError::BrokerUnavailable { .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Service access integration unavailable",
            ),
        };
        // Domain Display deliberately omits driver/crypto sources and all secrets.
        if status.is_server_error() {
            tracing::error!(operation = "service_access", error = %error, "Service access request failed");
        }
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

pub(super) async fn audit(
    state: &AccessAppState,
    auth: &temps_authz::AuthContext,
    metadata: RequestMetadata,
    project_id: impl Into<Option<i32>>,
    resource_id: &str,
    version: i32,
    operation: &'static str,
) {
    let project_id = project_id.into();
    let event = AccessAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        project_id,
        resource_id: resource_id.into(),
        version,
        operation,
    };
    if state.audit_service.create_audit_log(&event).await.is_err() {
        tracing::error!(
            ?project_id,
            resource_id,
            operation,
            "Failed to record service access audit event"
        );
    }
}

// A denied write is a read-only capability, not a failed capability read.
// Infrastructure failures must remain errors rather than claiming a restriction.
fn permission_available(result: Result<(), Problem>) -> Result<bool, Problem> {
    match result {
        Ok(()) => Ok(true),
        Err(problem) if problem.status_code == StatusCode::FORBIDDEN => Ok(false),
        Err(problem) => Err(problem),
    }
}

#[utoipa::path(get, path="/projects/{project_id}/service-access/capability", tag="Sandbox service access", params(("project_id" = i32, Path, description="Project ID")),
responses((status=200, description="Success", body=AccessCapability), (status=400, description="Invalid request"), (status=401, description="Unauthorized"), (status=403, description="Forbidden"), (status=404, description="Resource not found"), (status=409, description="Version conflict"), (status=500, description="Storage failure")), security(("bearer_auth"=[])))]
pub async fn access_capability(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsRead,
        project_id,
        &state.project_access_checker
    );
    let mut capability = state.service.capability();
    let project_write = permission_available(
        async {
            project_permission_guard!(
                auth,
                ProjectsWrite,
                project_id,
                &state.project_access_checker
            );
            Ok(())
        }
        .await,
    )?;
    capability.can_manage_services = auth.is_admin() && project_write;
    capability.can_manage_requests = project_write
        && permission_available(
            async {
                project_permission_guard!(
                    auth,
                    SandboxesWrite,
                    project_id,
                    &state.project_access_checker
                );
                Ok(())
            }
            .await,
        )?;
    capability.can_use_project_github = capability.can_manage_services
        && auth.has_permission(&temps_authz::Permission::GitRepositoriesRead)
        && auth.has_permission(&temps_authz::Permission::GitConnectionsRead)
        && permission_available(
            async {
                project_permission_guard!(
                    auth,
                    GitRepositoriesRead,
                    project_id,
                    &state.project_access_checker
                );
                Ok(())
            }
            .await,
        )?;
    Ok(Json(capability))
}

#[utoipa::path(get, path="/projects/{project_id}/access-services", tag="Sandbox service access", params(("project_id" = i32, Path, description="Project ID"), AccessPageQuery),
responses((status=200, description="Success", body=AccessServiceList), (status=400, description="Invalid request"), (status=401, description="Unauthorized"), (status=403, description="Forbidden"), (status=404, description="Resource not found"), (status=409, description="Version conflict"), (status=500, description="Storage failure")), security(("bearer_auth"=[])))]
pub async fn list_access_services(
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
    Ok(Json(state.service.list_services(project_id, query).await?))
}

#[utoipa::path(get, path="/projects/{project_id}/access-services/{service_id}", tag="Sandbox service access", params(("project_id" = i32, Path, description="Project ID"), ("service_id" = String, Path, description="Service ID")),
responses((status=200, description="Service policy without stored credential", body=AccessServiceResponse), (status=401, description="Unauthorized"), (status=403, description="Forbidden"), (status=404, description="Resource not found"), (status=500, description="Storage failure")), security(("bearer_auth"=[])))]
pub async fn get_access_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, service_id)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsRead,
        project_id,
        &state.project_access_checker
    );
    Ok(Json(
        state.service.get_service(project_id, &service_id).await?,
    ))
}

#[utoipa::path(post, path="/projects/{project_id}/access-services", tag="Sandbox service access", params(("project_id" = i32, Path, description="Project ID")),
request_body=SaveAccessService,
responses((status=201, description="Success", body=AccessServiceResponse), (status=400, description="Invalid request"), (status=401, description="Unauthorized"), (status=403, description="Forbidden"), (status=404, description="Resource not found"), (status=409, description="Version conflict"), (status=500, description="Storage failure")), security(("bearer_auth"=[])))]
pub async fn create_access_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessService>,
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
    if request.policy.github_repository.is_some() {
        permission_guard!(auth, GitRepositoriesRead);
        permission_guard!(auth, GitConnectionsRead);
        project_permission_guard!(
            auth,
            GitRepositoriesRead,
            project_id,
            &state.project_access_checker
        );
    }
    let response = state
        .service
        .save_service(project_id, None, request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "SANDBOX_ACCESS_SERVICE_CREATED",
    )
    .await;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(put, path="/projects/{project_id}/access-services/{service_id}", tag="Sandbox service access", params(("project_id" = i32, Path, description="Project ID"), ("service_id" = String, Path, description="Service ID")),
request_body=SaveAccessService,
responses((status=200, description="Success", body=AccessServiceResponse), (status=400, description="Invalid request"), (status=401, description="Unauthorized"), (status=403, description="Forbidden"), (status=404, description="Resource not found"), (status=409, description="Version conflict"), (status=500, description="Storage failure")), security(("bearer_auth"=[])))]
pub async fn update_access_service(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, service_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SaveAccessService>,
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
    if request.policy.github_repository.is_some() {
        permission_guard!(auth, GitRepositoriesRead);
        permission_guard!(auth, GitConnectionsRead);
        project_permission_guard!(
            auth,
            GitRepositoriesRead,
            project_id,
            &state.project_access_checker
        );
    }
    let response = state
        .service
        .save_service(project_id, Some(service_id), request)
        .await?;
    audit(
        &state,
        &auth,
        metadata,
        project_id,
        &response.id,
        response.version,
        "SANDBOX_ACCESS_SERVICE_UPDATED",
    )
    .await;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(get, path="/projects/{project_id}/sandboxes/{sandbox_id}/service-grants", tag="Sandbox service access", params(("project_id" = i32, Path, description="Project ID"), ("sandbox_id" = String, Path, description="Opaque sandbox ID"), AccessPageQuery),
responses((status=200, description="Success", body=ServiceGrantList), (status=400, description="Invalid request"), (status=401, description="Unauthorized"), (status=403, description="Forbidden"), (status=404, description="Resource not found"), (status=409, description="Version conflict"), (status=500, description="Storage failure")), security(("bearer_auth"=[])))]
pub async fn list_service_grants(
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
    Ok(Json(
        state
            .service
            .list_grants(
                project_id,
                &sandbox_id,
                auth.user_id(),
                auth.is_admin(),
                query,
            )
            .await?,
    ))
}

#[utoipa::path(post, path="/projects/{project_id}/sandboxes/{sandbox_id}/service-grants", tag="Sandbox service access", params(("project_id" = i32, Path, description="Project ID"), ("sandbox_id" = String, Path, description="Opaque sandbox ID")),
request_body=RequestServiceGrant,
responses((status=201, description="Success", body=ServiceGrantResponse), (status=400, description="Invalid request"), (status=401, description="Unauthorized"), (status=403, description="Forbidden"), (status=404, description="Resource not found"), (status=409, description="Version conflict"), (status=500, description="Storage failure")), security(("bearer_auth"=[])))]
pub async fn request_service_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, sandbox_id)): Path<(i32, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RequestServiceGrant>,
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
        .request_grant(
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
        "SANDBOX_SERVICE_GRANT_REQUESTED",
    )
    .await;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(post, path="/projects/{project_id}/sandboxes/{sandbox_id}/service-grants/{grant_id}/revoke", tag="Sandbox service access", params(("project_id" = i32, Path, description="Project ID"), ("sandbox_id" = String, Path, description="Opaque sandbox ID"), ("grant_id" = String, Path, description="Grant request ID")),
request_body=RevokeServiceGrant,
responses((status=200, description="Success", body=ServiceGrantResponse), (status=400, description="Invalid request"), (status=401, description="Unauthorized"), (status=403, description="Forbidden"), (status=404, description="Resource not found"), (status=409, description="Version conflict"), (status=500, description="Storage failure")), security(("bearer_auth"=[])))]
pub async fn revoke_service_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AccessAppState>>,
    Path((project_id, sandbox_id, grant_id)): Path<(i32, String, String)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RevokeServiceGrant>,
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
        .revoke_grant(
            project_id,
            &sandbox_id,
            &grant_id,
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
        "SANDBOX_SERVICE_GRANT_REVOKED",
    )
    .await;
    Ok((StatusCode::OK, Json(response)))
}

#[cfg(test)]
mod tests;
