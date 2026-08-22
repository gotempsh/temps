//! Email domain handlers

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth, Role};
use temps_core::{
    error_builder::{bad_request, forbidden, internal_server_error, not_found},
    problemdetails::{self, Problem},
    AuditContext, RequestMetadata,
};
use temps_dns::providers::{DnsProvider, DnsRecordContent, DnsRecordRequest};
use tracing::{error, info, warn};

use super::audit::{
    EmailDomainCreatedAudit, EmailDomainDeletedAudit, EmailDomainProjectAuthorizedAudit,
    EmailDomainProjectChangeRequestedAudit, EmailDomainProjectRevokedAudit,
    EmailDomainVerifiedAudit,
};
use super::types::{
    AppState, AuthorizedEmailDomainProjectResponse, CreateEmailDomainRequest, DnsRecordResponse,
    DnsRecordSetupResult, EmailDomainResponse, EmailDomainWithDnsResponse, ListDomainsQuery,
    SetupDnsRequest, SetupDnsResponse,
};
use crate::errors::EmailError;
use crate::services::CreateDomainRequest;

/// Map every EmailError variant to its correct HTTP status + real error message.
///
/// Previously every email-domain handler did a manual `map_err` that collapsed every
/// possible failure into `404 "Domain not found"` (or `500 "..."`), which masked real
/// problems like decryption failures and missing providers behind the wrong status
/// code. Keep this impl exhaustive so future variants force a conscious mapping.
impl From<EmailError> for Problem {
    fn from(error: EmailError) -> Self {
        match error {
            EmailError::DomainNotFound(_)
            | EmailError::ProviderNotFound(_)
            | EmailError::EmailNotFound(_)
            | EmailError::ProjectNotFound(_) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(error.to_string()),

            EmailError::DomainNotVerified(_) => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Domain Not Verified")
                .with_detail(error.to_string()),

            EmailError::DomainNotAuthorized { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Sender Domain Not Authorized")
                .with_detail(error.to_string()),

            EmailError::IdempotencyConflict { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Idempotency Key Conflict")
                .with_detail(error.to_string()),

            EmailError::Validation(_) | EmailError::InvalidProviderType(_) => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Validation Error")
                    .with_detail(error.to_string())
            }

            EmailError::Database(_)
            | EmailError::ProviderError(_)
            | EmailError::ProviderDeliveryUnknown(_)
            | EmailError::Encryption(_)
            | EmailError::Decryption(_)
            | EmailError::Configuration(_)
            | EmailError::AwsSes(_)
            | EmailError::Scaleway(_)
            | EmailError::ScalewayClientBuild { .. }
            | EmailError::Smtp(_)
            | EmailError::Serialization(_)
            | EmailError::TrackingRewrite { .. }
            | EmailError::SendFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Configure domain routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/email-domains",
            post(create_email_domain).get(list_email_domains),
        )
        .route("/email-domains/by-domain/{domain}", get(get_domain_by_name))
        .route(
            "/email-domains/{id}",
            get(get_domain).delete(delete_email_domain),
        )
        .route(
            "/email-domains/{id}/dns-records",
            get(get_domain_dns_records),
        )
        .route("/email-domains/{id}/verify", post(verify_domain))
        .route("/email-domains/{id}/setup-dns", post(setup_dns))
        .route(
            "/email-domains/{id}/projects",
            get(list_email_domain_projects),
        )
        .route(
            "/email-domains/{id}/projects/{project_id}",
            post(authorize_email_domain_project).delete(revoke_email_domain_project),
        )
}

#[utoipa::path(
    tag = "Email Domains",
    get,
    path = "/email-domains/{id}/projects",
    params(("id" = i32, Path, description = "Email domain ID")),
    responses(
        (status = 200, description = "Projects authorized to use this sender domain", body = [AuthorizedEmailDomainProjectResponse]),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Project visibility or database check failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_email_domain_projects(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<Json<Vec<AuthorizedEmailDomainProjectResponse>>, Problem> {
    permission_guard!(auth, EmailDomainsRead);
    let mut projects = state
        .domain_service
        .list_authorized_projects(id)
        .await
        .map_err(Problem::from)?;
    if !(auth.is_admin() || auth.has_role(&Role::PlatformAdmin)) {
        if let (Some(checker), Some(user_id)) =
            (state.project_access_checker.as_ref(), auth.user_id_opt())
        {
            let hidden = checker.hidden_project_ids(user_id).await.map_err(|error| {
                error!(
                    user_id,
                    domain_id = id,
                    error = %error,
                    "Project visibility check failed while listing email-domain grants"
                );
                internal_server_error()
                    .title("Project Access Check Failed")
                    .detail(format!(
                        "Could not verify project visibility for email domain {id}"
                    ))
                    .build()
            })?;
            if let Some(hidden) = hidden {
                let hidden = hidden.into_iter().collect::<std::collections::HashSet<_>>();
                projects.retain(|project| !hidden.contains(&project.id));
            }
        }
    }

    Ok(Json(projects.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    tag = "Email Domains",
    post,
    path = "/email-domains/{id}/projects/{project_id}",
    params(
        ("id" = i32, Path, description = "Email domain ID"),
        ("project_id" = i32, Path, description = "Project allowed to send from this domain")
    ),
    responses(
        (status = 204, description = "Project authorized"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only an instance or platform administrator may change global sender-domain grants"),
        (status = 404, description = "Domain or project not found"),
        (status = 500, description = "Project access, audit, or database check failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn authorize_email_domain_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Path((id, project_id)): Path<(i32, i32)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, EmailDomainsWrite);
    require_global_sender_domain_authority(&auth)?;
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    require_same_origin_session(&auth, &metadata)?;
    let correlation_id = require_domain_project_change_audit(
        state.as_ref(),
        &auth,
        &metadata,
        id,
        project_id,
        "authorize",
    )
    .await?;
    let result = state.domain_service.authorize_project(id, project_id).await;
    let audit = EmailDomainProjectAuthorizedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        correlation_id,
        domain_id: id,
        project_id,
        success: result.is_ok(),
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to audit email-domain project authorization: {error}");
    }
    result.map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Email Domains",
    delete,
    path = "/email-domains/{id}/projects/{project_id}",
    params(
        ("id" = i32, Path, description = "Email domain ID"),
        ("project_id" = i32, Path, description = "Project whose authorization is revoked")
    ),
    responses(
        (status = 204, description = "Project authorization revoked"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only an instance or platform administrator may change global sender-domain grants"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Project access, audit, or database check failed")
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_email_domain_project(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Path((id, project_id)): Path<(i32, i32)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, EmailDomainsWrite);
    require_global_sender_domain_authority(&auth)?;
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    require_same_origin_session(&auth, &metadata)?;
    let correlation_id = require_domain_project_change_audit(
        state.as_ref(),
        &auth,
        &metadata,
        id,
        project_id,
        "revoke",
    )
    .await?;
    let result = state.domain_service.revoke_project(id, project_id).await;
    let audit = EmailDomainProjectRevokedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        correlation_id,
        domain_id: id,
        project_id,
        success: result.is_ok(),
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to audit email-domain project revocation: {error}");
    }
    result.map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sender domains are instance-global resources. Project membership proves a
/// caller may operate a project, but it does not prove authority over an
/// arbitrary verified sender domain. Until domains have an explicit owner,
/// only an instance administrator may change domain-to-project grants.
fn require_global_sender_domain_authority(auth: &temps_auth::AuthContext) -> Result<(), Problem> {
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(());
    }

    Err(forbidden()
        .title("Sender Domain Authority Required")
        .detail(
            "Only an instance or platform administrator may change project access to a global sender domain",
        )
        .build())
}

async fn require_domain_project_change_audit(
    state: &AppState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    domain_id: i32,
    project_id: i32,
    action: &str,
) -> Result<uuid::Uuid, Problem> {
    let correlation_id = uuid::Uuid::new_v4();
    let audit = EmailDomainProjectChangeRequestedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        correlation_id,
        domain_id,
        project_id,
        action: action.to_string(),
    };

    state
        .audit_service
        .create_audit_log(&audit)
        .await
        .map_err(|error| {
            error!(
                "Refusing unaudited email-domain project {action} for domain {domain_id}, project {project_id}: {error}"
            );
            internal_server_error()
                .title("Audit Log Unavailable")
                .detail("The sender-domain change was not applied because its audit record could not be stored")
                .build()
        })?;

    Ok(correlation_id)
}

/// Browser sessions carry ambient cookie credentials, so unsafe requests must
/// originate from the exact console origin. A sibling deployment such as
/// `attacker.example.com` is same-site and can receive `SameSite=Strict`
/// cookies, but it is not same-origin. Bearer-authenticated API/CLI clients do
/// not use ambient credentials and therefore do not need browser origin
/// headers.
fn require_same_origin_session(
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
) -> Result<(), Problem> {
    if !auth.is_session() || request_is_same_origin(metadata) {
        return Ok(());
    }

    Err(forbidden()
        .detail("Browser session requests that change email-domain project access must originate from this Temps console")
        .build())
}

fn request_is_same_origin(metadata: &RequestMetadata) -> bool {
    let expected_origin = metadata.base_url.trim_end_matches('/');
    if metadata
        .headers
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin.trim_end_matches('/') == expected_origin)
    {
        return true;
    }

    metadata
        .headers
        .get("referer")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|referer| {
            referer == expected_origin
                || referer
                    .strip_prefix(expected_origin)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        })
}

/// Create a new email domain
#[utoipa::path(
    tag = "Email Domains",
    post,
    path = "/email-domains",
    request_body = CreateEmailDomainRequest,
    responses(
        (status = 201, description = "Domain created successfully", body = EmailDomainWithDnsResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_email_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Json(request): Json<CreateEmailDomainRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsCreate);

    let create_request = CreateDomainRequest {
        provider_id: request.provider_id,
        domain: request.domain.clone(),
    };

    let result = state
        .domain_service
        .create(create_request)
        .await
        .map_err(|e| {
            error!("Failed to create email domain: {}", e);
            internal_server_error()
                .detail(format!("Failed to create domain: {}", e))
                .build()
        })?;

    // Create audit log
    let audit = EmailDomainCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain_id: result.domain.id,
        domain: result.domain.domain.clone(),
        provider_id: result.domain.provider_id,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = EmailDomainWithDnsResponse {
        domain: EmailDomainResponse {
            id: result.domain.id,
            provider_id: result.domain.provider_id,
            domain: result.domain.domain,
            status: result.domain.status,
            last_verified_at: result.domain.last_verified_at.map(|dt| dt.to_rfc3339()),
            verification_error: result.domain.verification_error,
            created_at: result.domain.created_at.to_rfc3339(),
            updated_at: result.domain.updated_at.to_rfc3339(),
        },
        dns_records: result
            .dns_records
            .into_iter()
            .map(|r| DnsRecordResponse {
                record_type: r.record_type,
                name: r.name,
                value: r.value,
                priority: r.priority,
                status: r.status.into(),
            })
            .collect(),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// List all email domains
#[utoipa::path(
    tag = "Email Domains",
    get,
    path = "/email-domains",
    params(ListDomainsQuery),
    responses(
        (status = 200, description = "List of email domains", body = Vec<EmailDomainResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_email_domains(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListDomainsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsRead);

    let domains = match query.provider_id {
        Some(provider_id) => state.domain_service.list_by_provider(provider_id).await,
        None => state.domain_service.list().await,
    }
    .map_err(|e| {
        error!("Failed to list email domains: {}", e);
        internal_server_error()
            .detail("Failed to list domains")
            .build()
    })?;

    let responses: Vec<EmailDomainResponse> = domains
        .into_iter()
        .map(|d| EmailDomainResponse {
            id: d.id,
            provider_id: d.provider_id,
            domain: d.domain,
            status: d.status,
            last_verified_at: d.last_verified_at.map(|dt| dt.to_rfc3339()),
            verification_error: d.verification_error,
            created_at: d.created_at.to_rfc3339(),
            updated_at: d.updated_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(responses))
}

/// Get an email domain by ID with DNS records
#[utoipa::path(
    tag = "Email Domains",
    get,
    path = "/email-domains/{id}",
    responses(
        (status = 200, description = "Email domain details with DNS records", body = EmailDomainWithDnsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Domain ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsRead);

    let result = state.domain_service.get_with_dns_records(id).await?;

    let response = EmailDomainWithDnsResponse {
        domain: EmailDomainResponse {
            id: result.domain.id,
            provider_id: result.domain.provider_id,
            domain: result.domain.domain,
            status: result.domain.status,
            last_verified_at: result.domain.last_verified_at.map(|dt| dt.to_rfc3339()),
            verification_error: result.domain.verification_error,
            created_at: result.domain.created_at.to_rfc3339(),
            updated_at: result.domain.updated_at.to_rfc3339(),
        },
        dns_records: result
            .dns_records
            .into_iter()
            .map(|r| DnsRecordResponse {
                record_type: r.record_type,
                name: r.name,
                value: r.value,
                priority: r.priority,
                status: r.status.into(),
            })
            .collect(),
    };

    Ok(Json(response))
}

/// Get an email domain by domain name with DNS records
#[utoipa::path(
    tag = "Email Domains",
    get,
    path = "/email-domains/by-domain/{domain}",
    responses(
        (status = 200, description = "Email domain details with DNS records", body = EmailDomainWithDnsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name (e.g., 'mail.example.com')")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_domain_by_name(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(domain): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsRead);

    let domain_model = state
        .domain_service
        .find_by_domain_name(&domain)
        .await
        .map_err(|e| {
            error!("Failed to get email domain by name: {}", e);
            internal_server_error()
                .detail("Failed to get domain")
                .build()
        })?
        .ok_or_else(|| not_found().detail("Domain not found").build())?;

    let result = state
        .domain_service
        .get_with_dns_records(domain_model.id)
        .await
        .map_err(|e| {
            error!("Failed to get email domain DNS records: {}", e);
            internal_server_error()
                .detail("Failed to get domain DNS records")
                .build()
        })?;

    let response = EmailDomainWithDnsResponse {
        domain: EmailDomainResponse {
            id: result.domain.id,
            provider_id: result.domain.provider_id,
            domain: result.domain.domain,
            status: result.domain.status,
            last_verified_at: result.domain.last_verified_at.map(|dt| dt.to_rfc3339()),
            verification_error: result.domain.verification_error,
            created_at: result.domain.created_at.to_rfc3339(),
            updated_at: result.domain.updated_at.to_rfc3339(),
        },
        dns_records: result
            .dns_records
            .into_iter()
            .map(|r| DnsRecordResponse {
                record_type: r.record_type,
                name: r.name,
                value: r.value,
                priority: r.priority,
                status: r.status.into(),
            })
            .collect(),
    };

    Ok(Json(response))
}

/// Get DNS records for an email domain
#[utoipa::path(
    tag = "Email Domains",
    get,
    path = "/email-domains/{id}/dns-records",
    responses(
        (status = 200, description = "DNS records for the domain", body = Vec<DnsRecordResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Domain ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_domain_dns_records(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsRead);

    let result = state
        .domain_service
        .get_with_dns_records(id)
        .await
        .map_err(|e| {
            error!("Failed to get email domain DNS records: {}", e);
            not_found().detail("Domain not found").build()
        })?;

    let dns_records: Vec<DnsRecordResponse> = result
        .dns_records
        .into_iter()
        .map(|r| DnsRecordResponse {
            record_type: r.record_type,
            name: r.name,
            value: r.value,
            priority: r.priority,
            status: r.status.into(),
        })
        .collect();

    Ok(Json(dns_records))
}

/// Verify an email domain's DNS configuration
#[utoipa::path(
    tag = "Email Domains",
    post,
    path = "/email-domains/{id}/verify",
    responses(
        (status = 200, description = "Domain verification result with DNS records", body = EmailDomainWithDnsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Domain ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn verify_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsWrite);

    let result = state.domain_service.verify(id).await.map_err(|e| {
        error!("Failed to verify email domain: {}", e);
        internal_server_error()
            .detail(format!("Failed to verify domain: {}", e))
            .build()
    })?;

    // Create audit log
    let audit = EmailDomainVerifiedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain_id: result.domain.id,
        domain: result.domain.domain.clone(),
        status: result.domain.status.clone(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = EmailDomainWithDnsResponse {
        domain: EmailDomainResponse {
            id: result.domain.id,
            provider_id: result.domain.provider_id,
            domain: result.domain.domain,
            status: result.domain.status,
            last_verified_at: result.domain.last_verified_at.map(|dt| dt.to_rfc3339()),
            verification_error: result.domain.verification_error,
            created_at: result.domain.created_at.to_rfc3339(),
            updated_at: result.domain.updated_at.to_rfc3339(),
        },
        dns_records: result
            .dns_records
            .into_iter()
            .map(|r| DnsRecordResponse {
                record_type: r.record_type,
                name: r.name,
                value: r.value,
                priority: r.priority,
                status: r.status.into(),
            })
            .collect(),
    };

    Ok(Json(response))
}

/// Delete an email domain
#[utoipa::path(
    tag = "Email Domains",
    delete,
    path = "/email-domains/{id}",
    responses(
        (status = 204, description = "Domain deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Domain ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_email_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsDelete);

    // Get domain details before deletion for audit log
    let domain = state.domain_service.get(id).await.map_err(|e| {
        error!("Failed to get email domain: {}", e);
        not_found().detail("Domain not found").build()
    })?;

    state.domain_service.delete(id).await.map_err(|e| {
        error!("Failed to delete email domain: {}", e);
        internal_server_error()
            .detail("Failed to delete domain")
            .build()
    })?;

    // Create audit log
    let audit = EmailDomainDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain_id: domain.id,
        domain: domain.domain,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Setup DNS records for an email domain using a configured DNS provider
#[utoipa::path(
    tag = "Email Domains",
    post,
    path = "/email-domains/{id}/setup-dns",
    request_body = SetupDnsRequest,
    responses(
        (status = 200, description = "DNS records setup result", body = SetupDnsResponse),
        (status = 400, description = "Invalid request or DNS provider not configured"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Email Domain ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn setup_dns(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Json(request): Json<SetupDnsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsWrite);

    // Check if DNS provider service is available
    let dns_provider_service = state.dns_provider_service.as_ref().ok_or_else(|| {
        bad_request()
            .detail("DNS provider service is not configured")
            .build()
    })?;

    // Get the email domain with its DNS records
    let domain_with_dns = state
        .domain_service
        .get_with_dns_records(id)
        .await
        .map_err(|e| {
            error!("Failed to get email domain: {}", e);
            not_found().detail("Email domain not found").build()
        })?;

    // Bind use of the provider credentials to an active, verified zone that
    // authoritatively covers this email domain. The provider ID is caller
    // input and must not grant access to unrelated DNS credentials.
    let email_domain = &domain_with_dns.domain.domain;
    let verified_zone = dns_provider_service
        .find_verified_zone_for_provider(request.dns_provider_id, email_domain)
        .await
        .map_err(|error| {
            error!(
                provider_id = request.dns_provider_id,
                domain = %email_domain,
                %error,
                "Failed to verify DNS provider authorization"
            );
            internal_server_error()
                .detail("Failed to verify DNS provider authorization for this domain")
                .build()
        })?;
    let Some(verified_zone) = verified_zone else {
        warn!(
            provider_id = request.dns_provider_id,
            domain = %email_domain,
            "Rejected email DNS setup through an unrelated provider"
        );
        return Err(bad_request()
            .detail(format!(
                "DNS provider {} is not authorized to manage {}",
                request.dns_provider_id, email_domain
            ))
            .build());
    };
    let base_domain = verified_zone
        .domain
        .trim()
        .trim_end_matches('.')
        .trim_start_matches("*.")
        .to_ascii_lowercase();

    // The authorization query above also requires this provider to be active.
    let dns_provider = dns_provider_service
        .get(request.dns_provider_id)
        .await
        .map_err(|e| {
            error!("Failed to get DNS provider: {}", e);
            not_found().detail("DNS provider not found").build()
        })?;

    // Create DNS provider instance
    let provider_instance = dns_provider_service
        .create_provider_instance(&dns_provider)
        .map_err(|e| {
            error!("Failed to create DNS provider instance: {}", e);
            internal_server_error()
                .detail(format!("Failed to initialize DNS provider: {}", e))
                .build()
        })?;

    // `email_domain` and `base_domain` are already in scope from the
    // provider-authorization check above (derived from the caller-verified
    // DNS zone, not re-derived here) — do not shadow them.

    // Create each DNS record — except DMARC. Unlike SPF/DKIM/MX, DMARC isn't
    // additive: publishing `_dmarc.<root-domain>` sets a `p=quarantine`
    // policy for the *entire* domain, which can affect mail from senders
    // other than Temps (e.g. the company's regular Google Workspace/M365
    // mail) if their SPF/DKIM alignment isn't already clean. Bundling that
    // into the same "create all records" click as the purely-additive
    // records would be exactly the kind of silent-on-the-user's-behalf
    // change CLAUDE.md's operator-control rule warns against, so DMARC stays
    // informational-only here — surfaced for the operator to add manually
    // once they've confirmed it's safe for their domain.
    let auto_creatable_records: Vec<_> = domain_with_dns
        .dns_records
        .iter()
        .filter(|r| !r.name.starts_with("_dmarc."))
        .collect();

    info!(
        "Setting up {} DNS records for {} using provider {}",
        auto_creatable_records.len(),
        email_domain,
        dns_provider.name
    );

    let mut results = Vec::new();
    let mut records_created: u32 = 0;

    for dns_record in auto_creatable_records {
        let result = create_dns_record(provider_instance.as_ref(), &base_domain, dns_record).await;

        if result.success {
            records_created += 1;
        }

        results.push(result);
    }

    let total_records = results.len() as u32;
    let all_success = records_created == total_records;

    let message = if all_success {
        format!(
            "Successfully created all {} DNS records for {}",
            total_records, email_domain
        )
    } else {
        format!(
            "Created {} of {} DNS records for {}. Some records may need manual configuration.",
            records_created, total_records, email_domain
        )
    };

    info!("{}", message);

    let response = SetupDnsResponse {
        success: all_success,
        records_created,
        total_records,
        results,
        message,
    };

    Ok(Json(response))
}

/// Create a single DNS record using the provider
async fn create_dns_record(
    provider: &dyn DnsProvider,
    base_domain: &str,
    record: &crate::providers::DnsRecord,
) -> DnsRecordSetupResult {
    // Convert the record type and value to DNS provider format
    let content = match record.record_type.to_uppercase().as_str() {
        "TXT" => DnsRecordContent::TXT {
            content: record.value.clone(),
        },
        "CNAME" => DnsRecordContent::CNAME {
            target: record.value.clone(),
        },
        "MX" => DnsRecordContent::MX {
            priority: record.priority.unwrap_or(10),
            target: record.value.clone(),
        },
        _ => {
            warn!(
                "Unsupported record type for automatic setup: {}",
                record.record_type
            );
            return DnsRecordSetupResult {
                record_type: record.record_type.clone(),
                name: record.name.clone(),
                success: false,
                automatic: false,
                message: format!(
                    "Unsupported record type: {}. Please configure manually.",
                    record.record_type
                ),
            };
        }
    };

    // Convert the record name to relative format (remove base domain suffix if present)
    let relative_name = if record.name.ends_with(base_domain) {
        let without_suffix = record
            .name
            .trim_end_matches(base_domain)
            .trim_end_matches('.');
        if without_suffix.is_empty() {
            "@".to_string()
        } else {
            without_suffix.to_string()
        }
    } else {
        record.name.clone()
    };

    let request = DnsRecordRequest {
        name: relative_name.clone(),
        content,
        ttl: Some(300), // 5 minutes TTL
        proxied: false,
    };

    match provider.set_record(base_domain, request).await {
        Ok(_) => {
            info!(
                "Successfully created {} record for {} in {}",
                record.record_type, record.name, base_domain
            );
            DnsRecordSetupResult {
                record_type: record.record_type.clone(),
                name: record.name.clone(),
                success: true,
                automatic: true,
                message: format!("Successfully created {} record", record.record_type),
            }
        }
        Err(e) => {
            warn!(
                "Failed to create {} record for {}: {}",
                record.record_type, record.name, e
            );
            DnsRecordSetupResult {
                record_type: record.record_type.clone(),
                name: record.name.clone(),
                success: false,
                automatic: false,
                message: format!("Failed to create record: {}", e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{request_is_same_origin, require_global_sender_domain_authority};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use temps_auth::{AuthContext, Role};
    use temps_core::RequestMetadata;

    fn auth_with_role(role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            temps_entities::users::Model {
                id: 42,
                name: "Domain Operator".to_string(),
                email: "operator@example.test".to_string(),
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

    fn metadata_with_header(name: &'static str, value: &'static str) -> RequestMetadata {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static(value));
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test".to_string(),
            headers,
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://app.example.com".to_string(),
            scheme: "https".to_string(),
            host: "app.example.com".to_string(),
            is_secure: true,
        }
    }

    #[test]
    fn accepts_exact_console_origin() {
        let metadata = metadata_with_header("origin", "https://app.example.com");
        assert!(request_is_same_origin(&metadata));
    }

    #[test]
    fn rejects_same_site_sibling_origin() {
        let metadata = metadata_with_header("origin", "https://attacker.example.com");
        assert!(!request_is_same_origin(&metadata));
    }

    #[test]
    fn accepts_same_origin_referer_fallback() {
        let metadata = metadata_with_header(
            "referer",
            "https://app.example.com/settings/email/domains/7",
        );
        assert!(request_is_same_origin(&metadata));
    }

    #[test]
    fn rejects_missing_browser_origin_evidence() {
        let metadata = metadata_with_header("accept", "application/json");
        assert!(!request_is_same_origin(&metadata));
    }

    #[test]
    fn only_instance_or_platform_admin_can_change_global_sender_domain_grants() {
        assert!(require_global_sender_domain_authority(&auth_with_role(Role::Admin)).is_ok());
        assert!(
            require_global_sender_domain_authority(&auth_with_role(Role::PlatformAdmin)).is_ok()
        );
        let denied = require_global_sender_domain_authority(&auth_with_role(Role::User))
            .expect_err("project membership must not confer authority over a global domain");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }
}
