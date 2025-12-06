//! Email domain handlers

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::{internal_server_error, not_found},
    problemdetails::Problem,
    AuditContext, RequestMetadata,
};
use tracing::error;

use super::audit::{EmailDomainCreatedAudit, EmailDomainDeletedAudit, EmailDomainVerifiedAudit};
use super::types::{
    AppState, CreateEmailDomainRequest, DnsRecordResponse, EmailDomainResponse,
    EmailDomainWithDnsResponse,
};
use crate::services::CreateDomainRequest;

/// Configure domain routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/email-domains", post(create_domain).get(list_domains))
        .route("/email-domains/by-domain/{domain}", get(get_domain_by_name))
        .route("/email-domains/{id}", get(get_domain).delete(delete_domain))
        .route(
            "/email-domains/{id}/dns-records",
            get(get_domain_dns_records),
        )
        .route("/email-domains/{id}/verify", post(verify_domain))
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
pub async fn create_domain(
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
    responses(
        (status = 200, description = "List of email domains", body = Vec<EmailDomainResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_domains(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailDomainsRead);

    let domains = state.domain_service.list().await.map_err(|e| {
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

    let result = state
        .domain_service
        .get_with_dns_records(id)
        .await
        .map_err(|e| {
            error!("Failed to get email domain: {}", e);
            not_found().detail("Domain not found").build()
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
pub async fn delete_domain(
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
