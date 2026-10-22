// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for import operations

pub mod audit;
pub mod types;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use temps_auth::{permission_check, AuthContext, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::{PermissionDenialKind, Problem};
use temps_core::{AuditContext, RequestMetadata};
use tracing::error;
use utoipa::OpenApi;

use crate::services::ImportExecutionScope;
use temps_import_types::ImportSource;
use types::{
    CreatePlanRequest, CreatePlanResponse, DiscoverRequest, DiscoverResponse, ExecuteImportRequest,
    ExecuteImportResponse, ImportSourceInfo, ImportStatusResponse,
};

/// Configure routes for the import API
pub fn configure_routes() -> Router<Arc<types::AppState>> {
    Router::new()
        .route("/imports/sources", get(list_sources))
        .route("/imports/discover", post(discover_workloads))
        .route("/imports/plan", post(create_plan))
        .route("/imports/execute", post(execute_import))
        .route("/imports/{session_id}", get(get_import_status))
}

/// Whether `source` reads this server's own host rather than a remote
/// platform reached with the caller's credentials. The Docker importer talks
/// to the local Docker daemon and ignores credentials entirely, so it sees
/// every container on the host -- every project's, and Temps' own -- with
/// their env vars, labels and bind-mount paths.
fn source_reads_local_host(source: ImportSource) -> bool {
    matches!(source, ImportSource::Docker)
}

/// A host-local source is an operator tool: only an instance administrator
/// may enumerate, inspect or import from it. Remote sources reach only what
/// the caller's own credentials can, so they keep the plain import
/// permissions.
fn require_source_access(auth: &AuthContext, source: ImportSource) -> Result<(), Problem> {
    if source_reads_local_host(source) && !auth.is_instance_admin() {
        return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/insufficient-permissions")
            .title("Instance Administrator Required")
            .detail(format!(
                "Importing from the '{}' source reads this server's Docker daemon, which \
                 holds every project's containers; only an instance administrator can use it",
                source.as_str()
            ))
            .value("source", source.as_str())
            .value("user_role", auth.effective_role.to_string())
            .permission_denial(PermissionDenialKind::InsufficientPermission, None)
            .build());
    }
    Ok(())
}

/// Create permissions a non-dry-run execution of `scope` requires on top of
/// `imports:create`. A dry run creates nothing, so it needs none.
fn required_execute_permissions(
    scope: &ImportExecutionScope,
    dry_run: bool,
) -> Vec<temps_auth::Permission> {
    if dry_run {
        return Vec::new();
    }
    // Every real execution creates a project and triggers its first deploy.
    let mut permissions = vec![
        temps_auth::Permission::ProjectsCreate,
        temps_auth::Permission::DeploymentsCreate,
    ];
    if scope.creates_services {
        permissions.push(temps_auth::Permission::ExternalServicesCreate);
    }
    if scope.creates_domains {
        permissions.push(temps_auth::Permission::DomainsCreate);
    }
    permissions
}

/// List available import sources
#[utoipa::path(
    get,
    path = "/imports/sources",
    tag = "Imports",
    responses(
        (status = 200, description = "List of available import sources", body = Vec<ImportSourceInfo>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_sources(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsRead);

    let sources = state.import_orchestrator.list_sources().await?;
    Ok(Json(sources))
}

/// Discover workloads from a source
#[utoipa::path(
    post,
    path = "/imports/discover",
    tag = "Imports",
    request_body = DiscoverRequest,
    responses(
        (status = 200, description = "List of discovered workloads", body = DiscoverResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn discover_workloads(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
    Json(request): Json<types::DiscoverRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsRead);
    require_source_access(&auth, request.source)?;

    let workloads = state
        .import_orchestrator
        .discover(request.source, &request.credentials, request.selector)
        .await?;

    Ok(Json(types::DiscoverResponse { workloads }))
}

/// Create an import plan
#[utoipa::path(
    post,
    path = "/imports/plan",
    tag = "Imports",
    request_body = CreatePlanRequest,
    responses(
        (status = 200, description = "Import plan created", body = CreatePlanResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn create_plan(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
    Json(request): Json<types::CreatePlanRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsCreate);
    require_source_access(&auth, request.source)?;

    let result = state
        .import_orchestrator
        .create_plan(
            auth.user_id(),
            request.source,
            request.workload_id,
            &request.credentials,
            request.repository_id,
        )
        .await?;

    Ok(Json(result))
}

/// Execute an import
#[utoipa::path(
    post,
    path = "/imports/execute",
    tag = "Imports",
    request_body = ExecuteImportRequest,
    responses(
        (status = 202, description = "Import execution started", body = ExecuteImportResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn execute_import(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<types::ExecuteImportRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsCreate);
    let dry_run = request.dry_run.unwrap_or(false);
    let scope = state
        .import_orchestrator
        .execution_scope(auth.user_id(), &request.session_id)?;
    // Re-checked here, not only at plan time: the caller may have lost
    // instance-admin standing since the session was planned.
    require_source_access(&auth, scope.source)?;
    // An import creates the project, its services and domains, and deploys
    // it. Each needs the permission it would need if made directly, so a
    // role or API key scoped to `imports:create` cannot use an import to
    // create what it was not granted.
    for permission in required_execute_permissions(&scope, dry_run) {
        permission_check!(auth, permission);
    }

    let session_id = request.session_id.clone();
    let project_name = request.project_name.clone();

    let result = state
        .import_orchestrator
        .execute_import(
            auth.user_id(),
            request.session_id,
            request.project_name,
            request.preset,
            request.directory,
            request.main_branch,
            dry_run,
        )
        .await?;

    // This creates real projects/services/deployments/domains from
    // user-supplied platform credentials — an audit trail is required
    // regardless of whether the import itself succeeded.
    let audit_event = audit::ImportExecutedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        session_id,
        project_name,
        project_id: result.project_id,
        environment_id: result.environment_id,
        dry_run,
        succeeded: matches!(result.status, types::ImportExecutionStatus::Completed),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit_event).await {
        error!("Failed to create audit log for import execution: {}", e);
        // Continue — the import itself already ran; audit logging failure
        // must not undo or fail a real operation that already happened.
    }

    Ok((StatusCode::ACCEPTED, Json(result)))
}

/// Get import status
#[utoipa::path(
    get,
    path = "/imports/{session_id}",
    tag = "Imports",
    params(
        ("session_id" = String, Path, description = "Import session ID")
    ),
    responses(
        (status = 200, description = "Import status", body = ImportStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Import session not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_import_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<types::AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, temps_auth::Permission::ImportsRead);

    let status = state
        .import_orchestrator
        .get_status(auth.user_id(), &session_id)
        .await?;
    Ok(Json(status))
}

/// OpenAPI documentation
#[derive(OpenApi)]
#[openapi(
    paths(
        list_sources,
        discover_workloads,
        create_plan,
        execute_import,
        get_import_status,
    ),
    components(schemas(
        types::ImportSourceInfo,
        types::ImportSourceCapabilities,
        types::DiscoverRequest,
        types::DiscoverResponse,
        types::CreatePlanRequest,
        types::CreatePlanResponse,
        types::ExecuteImportRequest,
        types::ExecuteImportResponse,
        types::ImportStatusResponse,
    )),
    tags(
        (name = "Imports", description = "Import workloads from external sources")
    )
)]
pub struct ImportApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    fn auth(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id: 11,
                name: "Import Tester".to_string(),
                email: "import-tester@example.com".to_string(),
                password_hash: None,
                email_verified: true,
                email_verification_token: None,
                email_verification_expires: None,
                password_reset_token: None,
                password_reset_expires: None,
                must_change_password: false,
                deleted_at: None,
                mfa_secret: None,
                mfa_enabled: false,
                mfa_recovery_codes: None,
                oidc_subject: None,
                oidc_provider_id: None,
                created_at: now,
                updated_at: now,
            },
            role,
        )
    }

    /// F2: users and readers hold `imports:read`/`imports:create`, but the
    /// Docker source would hand them every container on the host.
    #[test]
    fn non_admins_cannot_use_the_host_docker_source() {
        for role in [Role::User, Role::Reader] {
            let err = require_source_access(&auth(role), ImportSource::Docker)
                .expect_err("host Docker daemon is admin-only");
            assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        }
    }

    #[test]
    fn instance_admins_can_use_the_host_docker_source() {
        for role in [Role::Admin, Role::PlatformAdmin] {
            assert!(require_source_access(&auth(role), ImportSource::Docker).is_ok());
        }
    }

    #[test]
    fn remote_sources_keep_the_plain_import_permissions() {
        for source in [
            ImportSource::Coolify,
            ImportSource::Dokploy,
            ImportSource::Kubernetes,
            ImportSource::Caprover,
            ImportSource::Portainer,
            ImportSource::Kamal,
        ] {
            assert!(!source_reads_local_host(source), "{source:?}");
            assert!(require_source_access(&auth(Role::User), source).is_ok());
        }
    }

    fn scope(creates_services: bool, creates_domains: bool) -> ImportExecutionScope {
        ImportExecutionScope {
            source: ImportSource::Coolify,
            creates_services,
            creates_domains,
        }
    }

    /// F7: `imports:create` must not stand in for the create permissions of
    /// what the import makes.
    #[test]
    fn execution_requires_the_create_permissions_of_what_the_plan_makes() {
        use temps_auth::Permission;

        assert_eq!(
            required_execute_permissions(&scope(false, false), false),
            vec![Permission::ProjectsCreate, Permission::DeploymentsCreate]
        );
        assert_eq!(
            required_execute_permissions(&scope(true, true), false),
            vec![
                Permission::ProjectsCreate,
                Permission::DeploymentsCreate,
                Permission::ExternalServicesCreate,
                Permission::DomainsCreate,
            ]
        );
        assert!(required_execute_permissions(&scope(true, true), true).is_empty());
    }

    #[test]
    fn an_imports_only_api_key_cannot_execute_a_real_import() {
        let mut key = auth(Role::Custom);
        key.custom_permissions = Some(vec![
            temps_auth::Permission::ImportsRead,
            temps_auth::Permission::ImportsCreate,
        ]);
        let missing: Vec<_> = required_execute_permissions(&scope(true, true), false)
            .into_iter()
            .filter(|permission| !key.has_permission(permission))
            .collect();
        assert_eq!(missing.len(), 4, "{missing:?}");

        // A full user holds every one of them, so nothing changes for it.
        let user = auth(Role::User);
        assert!(required_execute_permissions(&scope(true, true), false)
            .iter()
            .all(|permission| user.has_permission(permission)));
    }
}
