// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::{
    AppState, CustomDomainRequest, CustomDomainResponse, CustomDomainWithInfo, DomainEnvironment,
    DomainInfo, ListCustomDomainsResponse, ReassignCustomDomainRequest, UpdateCustomDomainRequest,
};
use super::{
    audit::AuditContext, audit::CustomDomainReassignedAudit,
    audit::CustomDomainReassignmentRequestedAudit,
};
use crate::services::custom_domains::{CustomDomainEnrichment, CustomDomainError};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_permission_guard,
    RequireAuth,
};
use temps_core::problemdetails;
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use temps_entities::project_custom_domains;
use tracing::{error, info};
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    paths(
        create_custom_domain,
        get_custom_domain,
        list_custom_domains_for_project,
        update_custom_domain,
        get_custom_domain_by_hostname,
        reassign_custom_domain,
        delete_custom_domain,
        link_custom_domain_to_certificate,
    ),
    components(
        schemas(
            CustomDomainRequest,
            CustomDomainResponse,
            UpdateCustomDomainRequest,
            ReassignCustomDomainRequest,
            ListCustomDomainsResponse,
        )
    ),
    tags((name = "Custom Domains", description = "Custom domain management for projects"))
)]
pub struct CustomDomainsApiDoc;

/// Create a custom domain for a project
#[utoipa::path(
    post,
    path = "/{project_id}/custom-domains",
    request_body = CustomDomainRequest,
    responses(
        (status = 201, description = "Custom domain created successfully", body = CustomDomainResponse),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized"),
        (status = 409, description = "Domain already exists"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "Custom Domains",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_custom_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CustomDomainRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );

    info!(
        "Creating custom domain: {} for project: {}",
        request.domain, project_id
    );

    let custom_domain = state
        .custom_domain_service
        .create_custom_domain(
            project_id,
            request.environment_id,
            request.domain.clone(),
            request.redirect_to,
            request.status_code,
            request.branch,
            request.service_name,
        )
        .await?;

    // Fetch additional info for response
    let domain_with_info = state
        .custom_domain_service
        .get_domain_with_info(custom_domain)
        .await?;

    state
        .telemetry
        .report(temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::CustomDomainAdded,
        ));

    Ok((
        StatusCode::CREATED,
        Json(CustomDomainResponse::from(CustomDomainWithInfo::from(
            domain_with_info,
        ))),
    ))
}

/// Get a custom domain by ID
#[utoipa::path(
    get,
    path = "/{project_id}/custom-domains/{domain_id}",
    responses(
        (status = 200, description = "Custom domain retrieved successfully", body = CustomDomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Custom domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("domain_id" = i32, Path, description = "Custom domain ID")
    ),
    tag = "Custom Domains",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_custom_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, domain_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    info!(
        "Getting custom domain: {} for project: {}",
        domain_id, project_id
    );

    let custom_domain = state
        .custom_domain_service
        .get_custom_domain(domain_id)
        .await?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Custom domain not found")
                .with_detail(format!("Custom domain with ID {} not found", domain_id))
        })?;

    // Verify it belongs to the specified project
    if custom_domain.project_id != project_id {
        return Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Custom domain not found")
            .with_detail("Domain does not belong to the specified project"));
    }

    let domain_with_info = state
        .custom_domain_service
        .get_domain_with_info(custom_domain)
        .await?;

    Ok((
        StatusCode::OK,
        Json(CustomDomainResponse::from(CustomDomainWithInfo::from(
            domain_with_info,
        ))),
    ))
}

/// List all custom domains for a project
#[utoipa::path(
    get,
    path = "/{project_id}/custom-domains",
    responses(
        (status = 200, description = "Custom domains retrieved successfully", body = ListCustomDomainsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    tag = "Custom Domains",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_custom_domains_for_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    info!("Listing custom domains for project: {}", project_id);

    let custom_domains = state
        .custom_domain_service
        .list_custom_domains_for_project(project_id)
        .await?;

    let total = custom_domains.len();

    let mut domain_responses = Vec::new();
    for domain in custom_domains {
        let domain_with_info = state
            .custom_domain_service
            .get_domain_with_info(domain)
            .await?;
        domain_responses.push(CustomDomainResponse::from(CustomDomainWithInfo::from(
            domain_with_info,
        )));
    }

    Ok((
        StatusCode::OK,
        Json(ListCustomDomainsResponse {
            domains: domain_responses,
            total,
        }),
    ))
}

/// Find the project assignment for a certificate hostname.
#[utoipa::path(
    get,
    path = "/custom-domains/by-host/{hostname}",
    operation_id = "get_visible_custom_domain_by_hostname",
    responses(
        (status = 200, description = "Custom domain assignment retrieved", body = CustomDomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Project access denied"),
        (status = 404, description = "Domain is not assigned to a project")
    ),
    params(("hostname" = String, Path, description = "Domain hostname")),
    tag = "Custom Domains",
    security(("bearer_auth" = []))
)]
pub async fn get_custom_domain_by_hostname(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(hostname): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let hidden_project_ids = super::handlers::resolve_hidden_projects(&state, &auth).await?;

    let custom_domain = state
        .custom_domain_service
        .get_visible_custom_domain_by_hostname(&hostname, &hidden_project_ids)
        .await?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Domain is not assigned")
                .with_detail(format!(
                    "Domain {hostname} is not currently assigned to a project environment"
                ))
        })?;

    let domain_with_info = state
        .custom_domain_service
        .get_domain_with_info(custom_domain)
        .await?;
    Ok((
        StatusCode::OK,
        Json(CustomDomainResponse::from(CustomDomainWithInfo::from(
            domain_with_info,
        ))),
    ))
}

/// Move a domain between projects without deleting its route or certificate.
#[utoipa::path(
    put,
    path = "/{source_project_id}/custom-domains/{domain_id}/assignment",
    operation_id = "reassign_project_custom_domain",
    request_body = ReassignCustomDomainRequest,
    responses(
        (status = 200, description = "Domain assignment updated atomically", body = CustomDomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Write access required for both projects"),
        (status = 404, description = "Custom domain or target environment not found in the authorized project scopes"),
        (status = 409, description = "Domain assignment changed; refresh and retry")
    ),
    params(
        ("source_project_id" = i32, Path, description = "Current project ID"),
        ("domain_id" = i32, Path, description = "Custom domain ID")
    ),
    tag = "Custom Domains",
    security(("bearer_auth" = []))
)]
pub async fn reassign_custom_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((source_project_id, domain_id)): Path<(i32, i32)>,
    Json(request): Json<ReassignCustomDomainRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        source_project_id,
        state.project_access_checker
    );
    project_permission_guard!(
        auth,
        ProjectsWrite,
        request.target_project_id,
        state.project_access_checker
    );

    let audit = CustomDomainReassignmentRequestedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.to_string()),
            user_agent: metadata.user_agent,
        },
        custom_domain_id: domain_id,
        source_project_id,
        target_project_id: request.target_project_id,
        target_environment_id: request.target_environment_id,
    };
    let updated_domain = execute_reassignment(
        state.custom_domain_service.as_ref(),
        state.audit_service.as_ref(),
        audit,
    )
    .await?;

    let domain_with_info = state
        .custom_domain_service
        .get_domain_with_info(updated_domain)
        .await?;
    Ok((
        StatusCode::OK,
        Json(CustomDomainResponse::from(CustomDomainWithInfo::from(
            domain_with_info,
        ))),
    ))
}

async fn execute_reassignment(
    custom_domain_service: &crate::services::custom_domains::CustomDomainService,
    audit_service: &dyn temps_core::AuditLogger,
    audit: CustomDomainReassignmentRequestedAudit,
) -> Result<project_custom_domains::Model, CustomDomainError> {
    let domain_id = audit.custom_domain_id;
    let source_project_id = audit.source_project_id;
    let target_project_id = audit.target_project_id;
    let target_environment_id = audit.target_environment_id;

    // Validate scoped ownership before writing the durable REQUESTED audit.
    // Foreign and absent IDs deliberately produce the same not-found result.
    custom_domain_service
        .validate_reassignment(
            domain_id,
            source_project_id,
            target_project_id,
            target_environment_id,
        )
        .await?;

    // Persist a forensic intent before changing ownership. Audit backends may
    // fail independently from the project database, so mutation must not begin
    // until this record is durable.
    audit_service
        .create_audit_log(&audit)
        .await
        .map_err(|audit_error| {
            error!(
                domain_id,
                source_project_id,
                target_project_id,
                error = %audit_error,
                "Failed to persist required custom-domain reassignment audit intent"
            );
            CustomDomainError::AuditIntentFailed {
                domain_id,
                source_project_id,
                target_project_id,
                reason: audit_error.to_string(),
            }
        })?;

    let updated_domain = custom_domain_service
        .reassign_custom_domain(
            domain_id,
            source_project_id,
            target_project_id,
            target_environment_id,
        )
        .await?;

    let completed_audit = CustomDomainReassignedAudit {
        context: audit.context,
        custom_domain_id: updated_domain.id,
        domain: updated_domain.domain.clone(),
        source_project_id,
        target_project_id,
        target_environment_id,
    };
    if let Err(audit_error) = audit_service.create_audit_log(&completed_audit).await {
        error!(
            domain_id,
            source_project_id,
            target_project_id,
            error = %audit_error,
            "Failed to persist custom-domain reassignment completion audit"
        );
    }

    Ok(updated_domain)
}

/// Update a custom domain
#[utoipa::path(
    put,
    path = "/{project_id}/custom-domains/{domain_id}",
    request_body = UpdateCustomDomainRequest,
    responses(
        (status = 200, description = "Custom domain updated successfully", body = CustomDomainResponse),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Custom domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("domain_id" = i32, Path, description = "Custom domain ID")
    ),
    tag = "Custom Domains",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_custom_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, domain_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateCustomDomainRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );

    info!(
        "Updating custom domain: {} for project: {}",
        domain_id, project_id
    );

    // Verify domain belongs to project
    let existing_domain = state
        .custom_domain_service
        .get_custom_domain(domain_id)
        .await?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Custom domain not found")
                .with_detail(format!("Custom domain with ID {} not found", domain_id))
        })?;

    if existing_domain.project_id != project_id {
        return Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Custom domain not found")
            .with_detail("Domain does not belong to the specified project"));
    }

    // If only domain and/or environment_id are sent (no redirect fields), clear redirect settings
    let should_clear_redirect = request.redirect_to.is_none()
        && request.status_code.is_none()
        && request.branch.is_none()
        && (request.domain.is_some() || request.environment_id.is_some());

    let updated_domain = state
        .custom_domain_service
        .update_custom_domain(
            domain_id,
            request.domain,
            request.environment_id,
            request.redirect_to,
            request.status_code,
            request.branch,
            None,
            None,
            None,
            request.service_name,
        )
        .await?;

    // If we need to clear redirect settings, do a second update
    let updated_domain = if should_clear_redirect {
        state
            .custom_domain_service
            .update_custom_domain(
                domain_id,
                None,
                None,
                Some("".to_string()), // Empty string to clear
                Some(0),              // 0 to clear status code
                Some("".to_string()), // Empty string to clear branch
                None,
                None,
                None,
                None,
            )
            .await?
    } else {
        updated_domain
    };

    let domain_with_info = state
        .custom_domain_service
        .get_domain_with_info(updated_domain)
        .await?;

    Ok((
        StatusCode::OK,
        Json(CustomDomainResponse::from(CustomDomainWithInfo::from(
            domain_with_info,
        ))),
    ))
}

/// Delete a custom domain
#[utoipa::path(
    delete,
    path = "/{project_id}/custom-domains/{domain_id}",
    responses(
        (status = 204, description = "Custom domain deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Custom domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("domain_id" = i32, Path, description = "Custom domain ID")
    ),
    tag = "Custom Domains",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_custom_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, domain_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsDelete,
        project_id,
        state.project_access_checker
    );

    info!(
        "Deleting custom domain: {} for project: {}",
        domain_id, project_id
    );

    // Verify domain belongs to project
    let existing_domain = state
        .custom_domain_service
        .get_custom_domain(domain_id)
        .await?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Custom domain not found")
                .with_detail(format!("Custom domain with ID {} not found", domain_id))
        })?;

    if existing_domain.project_id != project_id {
        return Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Custom domain not found")
            .with_detail("Domain does not belong to the specified project"));
    }

    state
        .custom_domain_service
        .delete_custom_domain(domain_id)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Link a custom domain to a certificate
#[utoipa::path(
    post,
    path = "/{project_id}/custom-domains/{domain_id}/link-certificate/{certificate_id}",
    responses(
        (status = 200, description = "Custom domain linked to certificate successfully", body = CustomDomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Custom domain or certificate not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("domain_id" = i32, Path, description = "Custom domain ID"),
        ("certificate_id" = i32, Path, description = "Certificate ID")
    ),
    tag = "Custom Domains",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn link_custom_domain_to_certificate(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, domain_id, certificate_id)): Path<(i32, i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );

    info!(
        "Linking custom domain: {} to certificate: {} for project: {}",
        domain_id, certificate_id, project_id
    );

    // Verify domain belongs to project
    let existing_domain = state
        .custom_domain_service
        .get_custom_domain(domain_id)
        .await?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Custom domain not found")
                .with_detail(format!("Custom domain with ID {} not found", domain_id))
        })?;

    if existing_domain.project_id != project_id {
        return Err(problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Custom domain not found")
            .with_detail("Domain does not belong to the specified project"));
    }

    let updated_domain = state
        .custom_domain_service
        .link_certificate(domain_id, certificate_id)
        .await?;

    let domain_with_info = state
        .custom_domain_service
        .get_domain_with_info(updated_domain)
        .await?;

    Ok((
        StatusCode::OK,
        Json(CustomDomainResponse::from(CustomDomainWithInfo::from(
            domain_with_info,
        ))),
    ))
}

impl From<CustomDomainEnrichment> for CustomDomainWithInfo {
    fn from(value: CustomDomainEnrichment) -> Self {
        Self {
            custom_domain: value.custom_domain,
            domain_info: value.certificate.map(|certificate| DomainInfo {
                id: certificate.id,
                domain: certificate.domain,
                expiration_time: certificate.expiration_time,
                last_renewed: certificate.last_renewed,
            }),
            environment: value.environment.map(|environment| DomainEnvironment {
                id: environment.id,
                name: environment.name,
                slug: environment.slug,
            }),
        }
    }
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/custom-domains/by-host/{hostname}",
            get(get_custom_domain_by_hostname),
        )
        .route(
            "/projects/{project_id}/custom-domains",
            post(create_custom_domain).get(list_custom_domains_for_project),
        )
        .route(
            "/projects/{source_project_id}/custom-domains/{domain_id}/assignment",
            axum::routing::put(reassign_custom_domain),
        )
        .route(
            "/projects/{project_id}/custom-domains/{domain_id}",
            get(get_custom_domain)
                .put(update_custom_domain)
                .delete(delete_custom_domain),
        )
        .route(
            "/projects/{project_id}/custom-domains/{domain_id}/link-certificate/{certificate_id}",
            post(link_custom_domain_to_certificate),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use std::sync::Mutex;
    use temps_core::{AuditLogger, AuditOperation};
    use temps_entities::{environments, projects, upstream_config::UpstreamList};
    use temps_presets::PresetType;

    #[derive(Default)]
    struct RecordingAuditLogger {
        operations: Mutex<Vec<String>>,
        fail_requested: bool,
    }

    #[temps_core::async_trait::async_trait]
    impl AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn AuditOperation,
        ) -> Result<(), temps_core::anyhow::Error> {
            if self.fail_requested && operation.operation_type().ends_with("REQUESTED") {
                return Err(temps_core::anyhow::anyhow!("audit unavailable"));
            }
            self.operations
                .lock()
                .expect("audit recorder lock must not be poisoned")
                .push(operation.operation_type());
            Ok(())
        }
    }

    async fn test_database() -> Option<temps_database::test_utils::TestDatabase> {
        match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                println!("Docker not available, skipping");
                None
            }
            Err(error) => panic!("failed to create migrated test database: {error}"),
        }
    }

    async fn insert_project_environment(
        db: &sea_orm::DatabaseConnection,
        name: &str,
    ) -> (projects::Model, environments::Model) {
        let slug = name.to_ascii_lowercase().replace(' ', "-");
        let project = projects::ActiveModel {
            name: Set(name.to_string()),
            slug: Set(slug.clone()),
            repo_name: Set(format!("{slug}-repo")),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(PresetType::Static),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("test project must be inserted");
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set(slug.clone()),
            host: Set(format!("{slug}.temps.dev")),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("test environment must be inserted");
        (project, environment)
    }

    fn requested_audit(
        domain_id: i32,
        source_project_id: i32,
        target_project_id: i32,
        target_environment_id: i32,
    ) -> CustomDomainReassignmentRequestedAudit {
        CustomDomainReassignmentRequestedAudit {
            context: AuditContext {
                user_id: 1,
                ip_address: Some("127.0.0.1".to_string()),
                user_agent: "custom-domain-reassignment-test".to_string(),
            },
            custom_domain_id: domain_id,
            source_project_id,
            target_project_id,
            target_environment_id,
        }
    }

    #[tokio::test]
    async fn reassignment_persists_requested_then_completed_audits() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = crate::services::custom_domains::CustomDomainService::new(test_db.db.clone());
        let (source_project, source_environment) =
            insert_project_environment(test_db.db.as_ref(), "Audit Source").await;
        let (target_project, target_environment) =
            insert_project_environment(test_db.db.as_ref(), "Audit Target").await;
        let domain = service
            .create_custom_domain(
                source_project.id,
                source_environment.id,
                "audit-order.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("test domain must be created");
        let audit_logger = RecordingAuditLogger::default();

        let updated = execute_reassignment(
            &service,
            &audit_logger,
            requested_audit(
                domain.id,
                source_project.id,
                target_project.id,
                target_environment.id,
            ),
        )
        .await
        .expect("valid reassignment must succeed");

        assert_eq!(updated.project_id, target_project.id);
        assert_eq!(
            *audit_logger
                .operations
                .lock()
                .expect("audit recorder lock must not be poisoned"),
            vec![
                "CUSTOM_DOMAIN_REASSIGNMENT_REQUESTED".to_string(),
                "CUSTOM_DOMAIN_REASSIGNED".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn requested_audit_failure_prevents_reassignment() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = crate::services::custom_domains::CustomDomainService::new(test_db.db.clone());
        let (source_project, source_environment) =
            insert_project_environment(test_db.db.as_ref(), "Failing Audit Source").await;
        let (target_project, target_environment) =
            insert_project_environment(test_db.db.as_ref(), "Failing Audit Target").await;
        let domain = service
            .create_custom_domain(
                source_project.id,
                source_environment.id,
                "audit-failure.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("test domain must be created");
        let audit_logger = RecordingAuditLogger {
            fail_requested: true,
            ..Default::default()
        };

        let result = execute_reassignment(
            &service,
            &audit_logger,
            requested_audit(
                domain.id,
                source_project.id,
                target_project.id,
                target_environment.id,
            ),
        )
        .await;
        assert!(matches!(
            result,
            Err(CustomDomainError::AuditIntentFailed { .. })
        ));
        let persisted = service
            .get_custom_domain(domain.id)
            .await
            .expect("domain lookup must succeed")
            .expect("domain must still exist");
        assert_eq!(persisted.project_id, source_project.id);
        assert_eq!(persisted.environment_id, source_environment.id);
        assert!(audit_logger
            .operations
            .lock()
            .expect("audit recorder lock must not be poisoned")
            .is_empty());
    }

    #[tokio::test]
    async fn foreign_target_environment_is_rejected_before_audit() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = crate::services::custom_domains::CustomDomainService::new(test_db.db.clone());
        let (source_project, source_environment) =
            insert_project_environment(test_db.db.as_ref(), "Scoped Audit Source").await;
        let (target_project, _) =
            insert_project_environment(test_db.db.as_ref(), "Scoped Audit Target").await;
        let (_, foreign_environment) =
            insert_project_environment(test_db.db.as_ref(), "Scoped Audit Foreign").await;
        let domain = service
            .create_custom_domain(
                source_project.id,
                source_environment.id,
                "scoped-audit.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("test domain must be created");
        let audit_logger = RecordingAuditLogger::default();

        let result = execute_reassignment(
            &service,
            &audit_logger,
            requested_audit(
                domain.id,
                source_project.id,
                target_project.id,
                foreign_environment.id,
            ),
        )
        .await;

        assert!(matches!(result, Err(CustomDomainError::NotFound(_))));
        assert!(audit_logger
            .operations
            .lock()
            .expect("audit recorder lock must not be poisoned")
            .is_empty());
        let persisted = service
            .get_custom_domain(domain.id)
            .await
            .expect("domain lookup must succeed")
            .expect("domain must still exist");
        assert_eq!(persisted.project_id, source_project.id);
        assert_eq!(persisted.environment_id, source_environment.id);
    }

    #[test]
    fn test_get_custom_domain_by_hostname_handler_filters_inaccessible_projects_before_lookup() {
        let source = include_str!("custom_domains.rs");
        let start = source
            .find("pub async fn get_custom_domain_by_hostname")
            .expect("hostname handler must exist");
        let remainder = &source[start + 1..];
        let end = remainder.find("pub async fn").unwrap_or(remainder.len());
        let handler = &source[start..start + 1 + end];

        let deny_token = handler
            .find("deny_deployment_token!(auth)")
            .expect("hostname lookup must reject a deployment token without a visible-project set");
        let hidden_projects = handler
            .find("resolve_hidden_projects(&state, &auth)")
            .expect("hostname lookup must resolve projects hidden from the caller");
        let scoped_lookup = handler
            .find(".get_visible_custom_domain_by_hostname(&hostname, &hidden_project_ids)")
            .expect("hostname lookup must exclude hidden projects in its database query");

        assert!(deny_token < scoped_lookup);
        assert!(hidden_projects < scoped_lookup);
    }
}
