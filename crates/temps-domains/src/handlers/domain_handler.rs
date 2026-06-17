use super::types::{
    AcmeOrderResponse, ChallengeError, ChallengeValidationStatus, CreateDomainRequest,
    DnsChallengeRecordResult, DnsCompletionResponse, DomainAppState, DomainChallengeResponse,
    DomainError, DomainResponse, HttpChallengeDebugResponse, ListDomainsResponse,
    ListOrdersResponse, ProvisionResponse, SetupDnsChallengeRequest, SetupDnsChallengeResponse,
    TxtRecord,
};
use crate::tls::{ProviderError, RepositoryError, TlsError};
use crate::DomainServiceError;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, AuditOperation, RequestMetadata};

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use utoipa::OpenApi;

// ========================================
// Audit Types
// ========================================

#[derive(Debug, Clone, serde::Serialize)]
struct DomainAudit {
    context: AuditContext,
    domain: String,
    action: String,
}

impl AuditOperation for DomainAudit {
    fn operation_type(&self) -> String {
        self.action.clone()
    }
    fn user_id(&self) -> i32 {
        self.context.user_id
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

// Convert TlsError to Problem for consistent error handling
impl From<TlsError> for Problem {
    fn from(error: TlsError) -> Self {
        match error {
            TlsError::Repository(e) => Problem::from(e),
            TlsError::Provider(e) => Problem::from(e),
            TlsError::Dns(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("DNS Error")
                .detail(msg)
                .build(),
            TlsError::Validation(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Validation Error")
                .detail(msg)
                .build(),
            TlsError::NotFound(msg) => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Resource Not Found")
                .detail(msg)
                .build(),
            TlsError::Expired(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Certificate Expired")
                .detail(msg)
                .build(),
            TlsError::ManualActionRequired(msg) => ErrorBuilder::new(StatusCode::ACCEPTED)
                .title("Manual Action Required")
                .detail(msg)
                .build(),
            TlsError::Operation(msg) => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Operation Error")
                .detail(msg)
                .build(),
            TlsError::Configuration(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Configuration Error")
                .detail(msg)
                .build(),
            TlsError::Internal(msg) => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Internal Server Error")
                .detail(msg)
                .build(),
        }
    }
}

// Convert RepositoryError to Problem
impl From<RepositoryError> for Problem {
    fn from(error: RepositoryError) -> Self {
        match error {
            RepositoryError::Database(msg) => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Database Error")
                .detail(msg)
                .build(),
            RepositoryError::NotFound(msg) => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Resource Not Found")
                .detail(msg)
                .build(),
            RepositoryError::DuplicateEntry(msg) => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Duplicate Entry")
                .detail(msg)
                .build(),
            RepositoryError::InvalidData(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Invalid Data")
                .detail(msg)
                .build(),
            RepositoryError::Connection(msg) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Database Connection Error")
                    .detail(msg)
                    .build()
            }
            RepositoryError::Internal(msg) => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Internal Error")
                .detail(msg)
                .build(),
        }
    }
}

// Convert ProviderError to Problem
impl From<ProviderError> for Problem {
    fn from(error: ProviderError) -> Self {
        match error {
            ProviderError::Acme(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("ACME Error")
                .detail(msg)
                .build(),
            ProviderError::CertificateGeneration(msg) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Certificate Generation Error")
                    .detail(msg)
                    .build()
            }
            ProviderError::ChallengeFailed(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Challenge Failed")
                .detail(msg)
                .build(),
            ProviderError::ValidationFailed(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Validation Failed")
                .detail(msg)
                .build(),
            ProviderError::UnsupportedChallenge(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Unsupported Challenge Type")
                .detail(msg)
                .build(),
            ProviderError::Network(msg) => ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                .title("Network Error")
                .detail(msg)
                .build(),
            ProviderError::Configuration(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Configuration Error")
                .detail(msg)
                .build(),
            ProviderError::Internal(msg) => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Internal Provider Error")
                .detail(msg)
                .build(),
        }
    }
}

// Convert DomainServiceError to Problem
impl From<DomainServiceError> for Problem {
    fn from(error: DomainServiceError) -> Self {
        match error {
            DomainServiceError::Database(e) => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Database Error")
                .detail(e.to_string())
                .build(),
            DomainServiceError::NotFound(msg) => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Resource Not Found")
                .detail(msg)
                .build(),
            DomainServiceError::InvalidDomain(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Invalid Domain")
                .detail(msg)
                .build(),
            DomainServiceError::Challenge(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Challenge Error")
                .detail(msg)
                .build(),
            DomainServiceError::Tls(e) => Problem::from(e),
            DomainServiceError::Provider(e) => Problem::from(e),
            DomainServiceError::Repository(e) => Problem::from(e),
            DomainServiceError::Internal(msg) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Internal Server Error")
                    .detail(msg)
                    .build()
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_domain,
        get_domain_by_id,
        get_domain_by_host,
        provision_domain,
        check_domain_status,
        delete_domain,
        finalize_order,
        list_domains,
        renew_domain,
        get_challenge_token,
        create_or_recreate_order,
        cancel_domain_order,
        get_domain_order,
        list_orders,
        get_http_challenge_debug,
        setup_dns_challenge
    ),
    components(
        schemas(
            CreateDomainRequest,
            DomainResponse,
            DomainChallengeResponse,
            DnsCompletionResponse,
            TxtRecord,
            ProvisionResponse,
            ListDomainsResponse,
            DomainError,
            AcmeOrderResponse,
            ListOrdersResponse,
            HttpChallengeDebugResponse,
            ChallengeValidationStatus,
            ChallengeError,
            SetupDnsChallengeRequest,
            SetupDnsChallengeResponse,
            DnsChallengeRecordResult
        )
    ),
    info(
        title = "Domains API",
        description = "API endpoints for domain and SSL certificate management. \
        Handles domain registration, SSL provisioning, DNS challenges, and certificate renewal.",
        version = "1.0.0"
    ),
    tags(
        (name = "Domains", description = "Domain management endpoints")
    )
)]
pub struct DomainApiDoc;

/// Create a new domain
///
/// Creates a new domain and automatically requests a Let's Encrypt challenge.
/// You can specify the challenge type (HTTP-01 or DNS-01) in the request.
///
/// - **HTTP-01**: Validates domain ownership by placing a file on your web server at `/.well-known/acme-challenge/`
/// - **DNS-01**: Validates domain ownership by adding a TXT record to your DNS (required for wildcard domains)
#[utoipa::path(
    post,
    path = "/domains",
    request_body = CreateDomainRequest,
    responses(
        (status = 201, description = "Domain created successfully", body = DomainResponse),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_domain(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateDomainRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsCreate);

    // Validate that the user has an email configured
    // Deployment tokens are not allowed as we need a user email for Let's Encrypt
    let user = auth.require_user().map_err(|msg| {
        ErrorBuilder::new(StatusCode::FORBIDDEN)
            .title("User Required")
            .detail(msg)
            .build()
    })?;
    let user_email = &user.email;
    if user_email.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Email Required")
            .detail("Your account must have a valid email address to provision SSL certificates with Let's Encrypt")
            .build());
    }

    info!(
        "Creating new domain: {} with challenge type: {} for user: {}",
        request.domain, request.challenge_type, user_email
    );

    // Step 1: Create the domain in the database
    let domain = app_state
        .domain_service
        .create_domain(&request.domain, &request.challenge_type)
        .await
        .map_err(|e| {
            error!("Failed to create domain {}: {}", request.domain, e);
            e
        })?;

    info!(
        "Domain created successfully: {} with ID: {}",
        request.domain, domain.id
    );

    // Audit log
    let audit = DomainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain: request.domain.clone(),
        action: "DOMAIN_CREATED".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    // Step 2: Automatically request challenge for the domain
    match app_state
        .domain_service
        .request_challenge(&request.domain, user_email)
        .await
    {
        Ok(challenge_data) => {
            info!(
                "Challenge automatically requested for domain: {}. Challenge type: {}",
                request.domain, challenge_data.challenge_type
            );

            // Get updated domain with challenge information
            let updated_domain = app_state
                .domain_service
                .get_domain(&request.domain)
                .await
                .map_err(|e| {
                    error!("Failed to get updated domain {}: {}", request.domain, e);
                    e
                })?
                .unwrap(); // Safe because we just created it

            Ok((
                StatusCode::CREATED,
                Json(DomainResponse::from(updated_domain)),
            ))
        }
        Err(e) => {
            error!(
                "Failed to request challenge for domain {}: {}",
                request.domain, e
            );
            // Domain is still created, just challenge failed
            info!(
                "Domain {} created but challenge request failed - can be retried later",
                request.domain
            );
            Ok((StatusCode::CREATED, Json(DomainResponse::from(domain))))
        }
    }
}

/// Get domain by ID
#[utoipa::path(
    get,
    path = "/domains/{domain}",
    responses(
        (status = 200, description = "Domain retrieved successfully", body = DomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = i32, Path, description = "Domain ID")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_domain_by_id(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(domain_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    info!(
        "Getting domain by ID: {} for user: {}",
        domain_id,
        auth.user_id()
    );

    let domain = app_state
        .domain_service
        .get_domain_by_id(domain_id)
        .await
        .map_err(|e| {
            error!("Failed to get domain by ID {}: {}", domain_id, e);
            e
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain with ID {} not found", domain_id))
                .build()
        })?;

    info!(
        "Domain retrieved successfully. ID: {}, Domain: {}",
        domain_id, domain.domain
    );

    Ok((StatusCode::OK, Json(DomainResponse::from(domain))))
}

/// Get domain details by hostname
#[utoipa::path(
    get,
    path = "/domains/by-host/{hostname}",
    responses(
        (status = 200, description = "Domain details retrieved successfully", body = DomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("hostname" = String, Path, description = "Domain hostname")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_domain_by_host(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(hostname): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    info!(
        "Getting domain by hostname: {} for user: {}",
        hostname,
        auth.user_id()
    );

    let domain = app_state
        .domain_service
        .get_domain(&hostname)
        .await
        .map_err(|e| {
            error!("Failed to get domain by hostname {}: {}", hostname, e);
            e
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain {} not found", hostname))
                .build()
        })?;

    info!(
        "Domain retrieved successfully by hostname. Hostname: {}",
        hostname
    );

    Ok((StatusCode::OK, Json(DomainResponse::from(domain))))
}

/// Provision a domain certificate
#[utoipa::path(
    post,
    path = "/domains/{domain}/provision",
    responses(
        (status = 200, description = "Certificate provisioning initiated", body = ProvisionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn provision_domain(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(domain): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsWrite);

    // Validate that the user has an email configured
    // Deployment tokens are not allowed as we need a user email for Let's Encrypt
    let user = auth.require_user().map_err(|msg| {
        ErrorBuilder::new(StatusCode::FORBIDDEN)
            .title("User Required")
            .detail(msg)
            .build()
    })?;
    let user_email = &user.email;
    if user_email.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Email Required")
            .detail("Your account must have a valid email address to provision SSL certificates with Let's Encrypt")
            .build());
    }

    // Look up the domain to check the stored verification method
    let domain_model = app_state
        .domain_service
        .get_domain(&domain)
        .await
        .map_err(|e| {
            error!("Failed to get domain {}: {}", domain, e);
            e
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain {} not found", domain))
                .build()
        })?;

    // For DNS-01 domains, use request_challenge + complete_challenge flow
    if domain_model.verification_method == "dns-01" {
        info!(
            "Starting DNS-01 challenge provisioning for domain: {} for user: {}",
            domain,
            auth.user_id()
        );

        // Try to complete the DNS-01 challenge (assumes TXT records are already set)
        let result = app_state
            .domain_service
            .complete_challenge(&domain, user_email)
            .await;

        let result = match result {
            Ok(certificate) => {
                info!(
                    "Certificate successfully provisioned via DNS-01 for {}",
                    domain
                );
                app_state.telemetry.report(
                    temps_core::telemetry::TelemetryEvent::new(
                        temps_core::telemetry::TelemetryEventKind::SslCertificateIssued,
                    )
                    .with("success", true)
                    .with(
                        "verification_method",
                        certificate.verification_method.clone(),
                    )
                    .with("is_wildcard", certificate.is_wildcard),
                );
                Ok((
                    StatusCode::OK,
                    Json(ProvisionResponse::Complete(DomainResponse::from(
                        certificate,
                    ))),
                ))
            }
            Err(e) => {
                error!(
                    "Failed to provision certificate via DNS-01 for {}: {}",
                    domain, e
                );
                Ok((
                    StatusCode::OK,
                    Json(ProvisionResponse::Error(DomainError {
                        message: e.to_string(),
                        code: "PROVISION_FAILED".to_string(),
                        details: Some("DNS-01 challenge provisioning failed. Ensure TXT records are set correctly.".to_string()),
                    })),
                ))
            }
        };

        // Audit log
        let audit = DomainAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            domain: domain.clone(),
            action: "DOMAIN_PROVISIONED".to_string(),
        };
        if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
            error!("Failed to create audit log: {}", e);
        }

        return result;
    }

    // HTTP-01 flow (default)
    info!(
        "Starting HTTP-01 challenge provisioning for domain: {} for user: {}",
        domain,
        auth.user_id()
    );

    let result = match app_state
        .tls_service
        .provision_certificate(&domain, user_email)
        .await
    {
        Ok(certificate) => {
            info!("Certificate successfully provisioned for {}", domain);
            app_state.telemetry.report(
                temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::SslCertificateIssued,
                )
                .with("success", true)
                .with(
                    "verification_method",
                    certificate.verification_method.clone(),
                )
                .with("is_wildcard", certificate.is_wildcard),
            );
            Ok((
                StatusCode::OK,
                Json(ProvisionResponse::Complete(DomainResponse::from(
                    certificate,
                ))),
            ))
        }
        Err(TlsError::Provider(crate::tls::ProviderError::ChallengeFailed(msg))) => {
            info!(
                "HTTP challenge requires manual intervention for {}: {}",
                domain, msg
            );

            let challenge_response = DomainChallengeResponse {
                domain: domain.clone(),
                txt_records: vec![TxtRecord {
                    name: format!("_acme-challenge.{}", domain),
                    value: "HTTP challenge - see domain validation instructions".to_string(),
                }],
                status: "pending_http".to_string(),
            };

            Ok((
                StatusCode::ACCEPTED,
                Json(ProvisionResponse::Pending(challenge_response)),
            ))
        }
        Err(e) => {
            error!("Failed to provision certificate for {}: {}", domain, e);
            Ok((
                StatusCode::OK,
                Json(ProvisionResponse::Error(DomainError {
                    message: e.to_string(),
                    code: "PROVISION_FAILED".to_string(),
                    details: Some("HTTP challenge provisioning failed".to_string()),
                })),
            ))
        }
    };

    // Audit log
    let audit = DomainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain: domain.clone(),
        action: "DOMAIN_PROVISIONED".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    result
}

/// Check domain status
#[utoipa::path(
    get,
    path = "/domains/{domain}/status",
    responses(
        (status = 200, description = "Domain status retrieved successfully", body = DomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = i32, Path, description = "Domain ID")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn check_domain_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(domain): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    info!(
        "Checking status for domain ID: {} for user: {}",
        domain,
        auth.user_id()
    );

    use crate::tls::models::CertificateFilter;

    // Get all certificates and find the one with matching ID
    let certificates = app_state
        .repository
        .list_certificates(CertificateFilter::default())
        .await?;

    // Find the certificate by converting to domain response and matching ID
    let domain_responses: Vec<DomainResponse> =
        certificates.into_iter().map(DomainResponse::from).collect();

    let domain_db = domain_responses
        .into_iter()
        .find(|d| d.id == domain)
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain with ID {} not found", domain))
                .build()
        })?;

    info!(
        "Domain status retrieved successfully. ID: {}, Domain: {}, Status: {}",
        domain, domain_db.domain, domain_db.status
    );

    Ok((StatusCode::OK, Json(domain_db)))
}

/// Finalize ACME order for a domain
///
/// Finalizes the ACME order by completing the challenge validation and requesting the certificate.
/// This should be called after the challenge has been set up (DNS record added or HTTP token served).
#[utoipa::path(
    post,
    path = "/domains/{domain_id}/order/finalize",
    responses(
        (status = 200, description = "Order finalized successfully", body = DomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain or order not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain_id" = i32, Path, description = "Domain ID")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn finalize_order(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(domain_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsWrite);

    // Validate that the user has an email configured
    // Deployment tokens are not allowed as we need a user email for Let's Encrypt
    let user = auth.require_user().map_err(|msg| {
        ErrorBuilder::new(StatusCode::FORBIDDEN)
            .title("User Required")
            .detail(msg)
            .build()
    })?;
    let user_email = &user.email;
    if user_email.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Email Required")
            .detail("Your account must have a valid email address to provision SSL certificates with Let's Encrypt")
            .build());
    }

    // Get domain name from ID
    let domain_model = app_state
        .domain_service
        .get_domain_by_id(domain_id)
        .await
        .map_err(|e| {
            error!("Failed to get domain by ID {}: {}", domain_id, e);
            e
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain with ID {} not found", domain_id))
                .build()
        })?;

    let domain_name = domain_model.domain.clone();
    info!(
        "Finalizing order for domain: {} (ID: {}) for user: {}",
        domain_name, domain_id, user_email
    );

    // Complete the challenge (after user has added DNS record or HTTP token is served)
    let domain = app_state
        .domain_service
        .complete_challenge(&domain_name, user_email)
        .await
        .map_err(|e| {
            error!("Failed to finalize order for domain {}: {}", domain_name, e);
            e
        })?;

    info!("Order finalized successfully for domain: {}", domain.domain);

    app_state.telemetry.report(
        temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::SslCertificateIssued,
        )
        .with("success", true)
        .with("verification_method", domain.verification_method.clone())
        .with("is_wildcard", domain.is_wildcard),
    );

    // Audit log
    let audit = DomainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain: domain_name,
        action: "DOMAIN_ORDER_FINALIZED".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::OK, Json(DomainResponse::from(domain))))
}

/// Cancel ACME order for a domain
///
/// Cancels the current ACME order for a domain and clears all challenge data.
/// This allows you to start over with a new order if the previous one failed or got stuck.
#[utoipa::path(
    delete,
    path = "/domains/{domain_id}/order",
    responses(
        (status = 200, description = "Order cancelled successfully", body = DomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain_id" = i32, Path, description = "Domain ID")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn cancel_domain_order(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(domain_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsWrite);

    // Get domain name from ID
    let domain_model = app_state
        .domain_service
        .get_domain_by_id(domain_id)
        .await
        .map_err(|e| {
            error!("Failed to get domain by ID {}: {}", domain_id, e);
            e
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain with ID {} not found", domain_id))
                .build()
        })?;

    let domain_name = domain_model.domain.clone();
    info!(
        "Cancelling order for domain: {} (ID: {})",
        domain_name, domain_id
    );

    let domain = app_state
        .domain_service
        .cancel_order(&domain_name)
        .await
        .map_err(|e| {
            error!("Failed to cancel order for domain {}: {}", domain_name, e);
            e
        })?;

    info!("Order cancelled successfully for domain: {}", domain.domain);

    // Audit log
    let audit = DomainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain: domain_name,
        action: "DOMAIN_ORDER_CANCELLED".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::OK, Json(DomainResponse::from(domain))))
}

/// Delete a domain
#[utoipa::path(
    delete,
    path = "/domains/{domain}",
    responses(
        (status = 204, description = "Domain deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn delete_domain(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(domain): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsDelete);

    info!("Deleting domain: {} for user: {}", domain, auth.user_id());

    app_state
        .domain_service
        .delete_domain(&domain)
        .await
        .map_err(|e| {
            error!("Failed to delete domain {}: {}", domain, e);
            e
        })?;

    info!("Domain {} deleted successfully", domain);

    // Audit log
    let audit = DomainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain: domain.clone(),
        action: "DOMAIN_DELETED".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Query parameters for listing domains
#[derive(Debug, Clone, serde::Deserialize, utoipa::IntoParams)]
pub struct ListDomainsParams {
    /// Page number (1-indexed)
    #[param(example = 1)]
    pub page: Option<u64>,
    /// Number of items per page (max 100)
    #[param(example = 20)]
    pub page_size: Option<u64>,
    /// Search domains by name (substring match)
    #[param(example = "example.com")]
    pub search: Option<String>,
}

impl ListDomainsParams {
    pub fn normalize(&self) -> (u64, u64) {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 100);
        (page, page_size)
    }
}

/// List all domains
#[utoipa::path(
    get,
    path = "/domains",
    responses(
        (status = 200, description = "Domains retrieved successfully", body = ListDomainsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ListDomainsParams,
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_domains(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Query(params): Query<ListDomainsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    let (page, page_size) = params.normalize();

    debug!("Listing domains for user: {}", auth.user_id());

    let (domains, total) = app_state
        .domain_service
        .list_domains_with_total(page, page_size, params.search.as_deref())
        .await
        .map_err(|e| {
            error!("Failed to list domains: {}", e);
            e
        })?;

    let domain_responses: Vec<DomainResponse> =
        domains.into_iter().map(DomainResponse::from).collect();

    debug!(
        "Domains retrieved successfully. Count: {}, Total: {}",
        domain_responses.len(),
        total
    );

    Ok((
        StatusCode::OK,
        Json(ListDomainsResponse {
            domains: domain_responses,
            total,
            page,
            page_size,
        }),
    ))
}

/// Renew domain certificate
///
/// For HTTP-01 domains: Automatically renews the certificate
/// For DNS-01 domains (wildcards): Creates a new ACME order and returns challenge data
#[utoipa::path(
    post,
    path = "/domains/{domain}/renew",
    responses(
        (status = 200, description = "Certificate renewal initiated", body = ProvisionResponse),
        (status = 202, description = "DNS challenge created - manual action required", body = DomainChallengeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn renew_domain(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(domain): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsWrite);

    // Validate that the user has an email configured
    // Deployment tokens are not allowed as we need a user email for Let's Encrypt
    let user = auth.require_user().map_err(|msg| {
        ErrorBuilder::new(StatusCode::FORBIDDEN)
            .title("User Required")
            .detail(msg)
            .build()
    })?;
    let user_email = &user.email;
    if user_email.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Email Required")
            .detail("Your account must have a valid email address to provision SSL certificates with Let's Encrypt")
            .build());
    }

    info!(
        "Renewing certificate for domain: {} for user: {}",
        domain,
        auth.user_id()
    );

    // Get the domain to check verification method
    let domain_model = app_state
        .domain_service
        .get_domain(&domain)
        .await
        .map_err(|e| {
            error!("Failed to get domain {}: {}", domain, e);
            e
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain {} not found", domain))
                .build()
        })?;

    // Audit log
    let audit = DomainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain: domain.clone(),
        action: "DOMAIN_RENEWED".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    // For DNS-01 domains (wildcards), use request_challenge to create a new order
    if domain_model.verification_method == "dns-01" {
        info!(
            "DNS-01 domain {} - creating new ACME order for renewal",
            domain
        );

        // Request challenge from Let's Encrypt (creates new ACME order)
        let challenge_data = app_state
            .domain_service
            .request_challenge(&domain, user_email)
            .await
            .map_err(|e| {
                error!(
                    "Failed to create renewal order for domain {}: {}",
                    domain, e
                );
                e
            })?;

        // Convert to response
        let txt_records = challenge_data
            .txt_records
            .into_iter()
            .map(|record| TxtRecord {
                name: record.name,
                value: record.value,
            })
            .collect();

        let challenge_response = DomainChallengeResponse {
            domain: challenge_data.domain,
            txt_records,
            status: challenge_data.status,
        };

        info!(
            "Renewal order created for DNS-01 domain: {}. User must update DNS TXT records and finalize.",
            domain
        );

        // Return 202 Accepted with challenge data
        return Ok((StatusCode::ACCEPTED, Json(challenge_response)).into_response());
    }

    // For HTTP-01 domains, renew via the order-based flow so the renewal is visible
    // and recoverable in the certificate management UI.
    //
    // We deliberately do NOT use `tls_service.renew_certificate` here: that path stores
    // challenge state in the standalone `http_challenges` table and never writes an
    // `acme_orders` row, so a renewal that needs validation leaves the domain stuck in
    // `challenge_requested` with no order for the UI to act on (no "Verify & finalize"
    // action). Instead we mirror the DNS-01 flow:
    //   1. `request_challenge` creates a fresh ACME order, persists its challenge data in
    //      `acme_orders.authorizations`, and sets the domain to `challenge_requested`.
    //   2. HTTP-01 needs no user action — the proxy already serves the token at
    //      `/.well-known/acme-challenge/{token}` — so we immediately attempt
    //      `complete_challenge` to accept the challenge and finalize.
    // If that immediate finalize fails (e.g. DNS not yet pointed here, or Let's Encrypt
    // validation is still propagating), the order is left in place so the user can retry
    // via the "Verify & finalize" action instead of being stranded.
    info!(
        "HTTP-01 domain {} - creating new ACME order for renewal",
        domain
    );

    let challenge_data = app_state
        .domain_service
        .request_challenge(&domain, user_email)
        .await
        .map_err(|e| {
            error!(
                "Failed to create renewal order for domain {}: {}",
                domain, e
            );
            e
        })?;

    // If the certificate was issued immediately (cached/valid authorization), we're done.
    if challenge_data.status == "completed" {
        info!(
            "Certificate renewed immediately for HTTP-01 domain: {}",
            domain
        );
        if let Some(renewed) = app_state.domain_service.get_domain(&domain).await? {
            app_state.telemetry.report(
                temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::SslCertificateIssued,
                )
                .with("success", true)
                .with("verification_method", renewed.verification_method.clone())
                .with("is_wildcard", renewed.is_wildcard),
            );
            return Ok((
                StatusCode::OK,
                Json(ProvisionResponse::Complete(DomainResponse::from(renewed))),
            )
                .into_response());
        }
    }

    // Accept the challenge and finalize the order. HTTP-01 requires no manual step.
    match app_state
        .domain_service
        .complete_challenge(&domain, user_email)
        .await
    {
        Ok(renewed) => {
            info!(
                "Certificate successfully renewed for HTTP-01 domain: {}",
                domain
            );
            app_state.telemetry.report(
                temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::SslCertificateIssued,
                )
                .with("success", true)
                .with("verification_method", renewed.verification_method.clone())
                .with("is_wildcard", renewed.is_wildcard),
            );
            Ok((
                StatusCode::OK,
                Json(ProvisionResponse::Complete(DomainResponse::from(renewed))),
            )
                .into_response())
        }
        Err(e) => {
            // Validation did not pass on the first attempt. The ACME order is still
            // pending and persisted, so surface it as a recoverable pending state rather
            // than a hard failure — the user can retry via "Verify & finalize".
            warn!(
                "Immediate HTTP-01 finalize failed for {}, leaving order pending for retry: {}",
                domain, e
            );

            let txt_records = challenge_data
                .txt_records
                .into_iter()
                .map(|record| TxtRecord {
                    name: record.name,
                    value: record.value,
                })
                .collect();

            let challenge_response = DomainChallengeResponse {
                domain: challenge_data.domain,
                txt_records,
                status: "challenge_requested".to_string(),
            };

            Ok((
                StatusCode::ACCEPTED,
                Json(ProvisionResponse::Pending(challenge_response)),
            )
                .into_response())
        }
    }
}

/// Get domain challenge details
#[utoipa::path(
    get,
    path = "/domains/{domain}/challenge",
    responses(
        (status = 200, description = "Challenge details retrieved successfully", body = DomainChallengeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain or challenge not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_domain_challenge(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(domain): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    debug!(
        "Getting challenge for domain: {} for user: {}",
        domain,
        auth.user_id()
    );

    // Get the challenge status from the domain service
    match app_state.domain_service.get_challenge_status(&domain).await {
        Ok(Some(challenge_data)) => {
            // Convert internal DnsTxtRecord to API TxtRecord
            let txt_records = challenge_data
                .txt_records
                .into_iter()
                .map(|record| TxtRecord {
                    name: record.name,
                    value: record.value,
                })
                .collect();

            let challenge_response = DomainChallengeResponse {
                domain: challenge_data.domain,
                txt_records,
                status: challenge_data.status,
            };

            debug!(
                "DNS challenge retrieved successfully for domain: {}",
                domain
            );
            Ok((StatusCode::OK, Json(challenge_response)))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Challenge not found")
            .detail(format!(
                "No active DNS challenge found for domain {}",
                domain
            ))
            .build()),
        Err(e) => {
            error!("Failed to get challenge for domain {}: {}", domain, e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get challenge")
                .detail(e.to_string())
                .build())
        }
    }
}

/// Get DNS completion status for a domain
#[utoipa::path(
    get,
    path = "/domains/{domain}/dns-completion",
    responses(
        (status = 200, description = "DNS completion status retrieved successfully", body = DnsCompletionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_dns_completion(
    RequireAuth(auth): RequireAuth,
    State(_app_state): State<Arc<DomainAppState>>,
    Path(domain): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    debug!(
        "Getting DNS completion for domain: {} for user: {}",
        domain,
        auth.user_id()
    );

    // Note: This method may not exist in TlsService, you may need to implement it
    // or use an alternative approach
    let completion = DnsCompletionResponse {
        domain: domain.clone(),
        status: "pending".to_string(),
    };

    debug!(
        "DNS completion retrieved successfully for domain: {}",
        domain
    );

    Ok((StatusCode::OK, Json(completion)))
}

/// Get challenge token for a domain (returns plain text token)
#[utoipa::path(
    get,
    path = "/domains/{domain}/challenge-token",
    responses(
        (status = 200, description = "Challenge token retrieved successfully", body = String),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Challenge not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_challenge_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(domain): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    info!(
        "Getting challenge token for domain: {} for user: {}",
        domain,
        auth.user_id()
    );

    // Get the DNS challenge data from the repository
    match app_state.repository.find_dns_challenge(&domain).await {
        Ok(Some(challenge_data)) => {
            info!(
                "Challenge token retrieved successfully for domain: {}",
                domain
            );
            Ok((
                StatusCode::OK,
                [("content-type", "text/plain")],
                challenge_data.txt_record_value,
            ))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Challenge not found")
            .detail(format!(
                "No active DNS challenge found for domain {}",
                domain
            ))
            .build()),
        Err(e) => {
            error!("Failed to get challenge token for domain {}: {}", domain, e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get challenge token")
                .detail(e.to_string())
                .build())
        }
    }
}

/// Create or recreate ACME order for a domain
///
/// Creates a new ACME order with Let's Encrypt for the specified domain.
/// If an order already exists, you should cancel it first using the cancel-order endpoint.
/// Returns the challenge details that need to be fulfilled (DNS record or HTTP token).
#[utoipa::path(
    post,
    path = "/domains/{domain_id}/order",
    responses(
        (status = 200, description = "Order created successfully", body = DomainChallengeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain_id" = i32, Path, description = "Domain ID")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_or_recreate_order(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(domain_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsWrite);

    // Validate that the user has an email configured
    // Deployment tokens are not allowed as we need a user email for Let's Encrypt
    let user = auth.require_user().map_err(|msg| {
        ErrorBuilder::new(StatusCode::FORBIDDEN)
            .title("User Required")
            .detail(msg)
            .build()
    })?;
    let user_email = &user.email;
    if user_email.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Email Required")
            .detail("Your account must have a valid email address to provision SSL certificates with Let's Encrypt")
            .build());
    }

    // Get domain name from ID
    let domain_model = app_state
        .domain_service
        .get_domain_by_id(domain_id)
        .await
        .map_err(|e| {
            error!("Failed to get domain by ID {}: {}", domain_id, e);
            e
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain with ID {} not found", domain_id))
                .build()
        })?;

    let domain_name = domain_model.domain.clone();
    info!(
        "Creating ACME order for domain: {} (ID: {}) for user: {}",
        domain_name, domain_id, user_email
    );

    // Request challenge from Let's Encrypt
    let challenge_data = app_state
        .domain_service
        .request_challenge(&domain_name, user_email)
        .await
        .map_err(|e| {
            error!("Failed to create order for domain {}: {}", domain_name, e);
            e
        })?;

    // Convert internal DnsTxtRecord to API TxtRecord
    let txt_records = challenge_data
        .txt_records
        .into_iter()
        .map(|record| TxtRecord {
            name: record.name,
            value: record.value,
        })
        .collect();

    let challenge_response = DomainChallengeResponse {
        domain: challenge_data.domain,
        txt_records,
        status: challenge_data.status,
    };

    info!(
        "Order created successfully for domain: {}. Challenge type: {}",
        domain_name, challenge_data.challenge_type
    );

    // Audit log
    let audit = DomainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain: domain_name,
        action: "DOMAIN_ORDER_CREATED".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::OK, Json(challenge_response)))
}

/// Get ACME order for a domain
#[utoipa::path(
    get,
    path = "/domains/{domain_id}/order",
    responses(
        (status = 200, description = "Order retrieved successfully", body = AcmeOrderResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Order not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain_id" = i32, Path, description = "Domain ID")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_domain_order(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(domain_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    info!("Getting ACME order for domain ID: {}", domain_id);

    let order = app_state
        .repository
        .find_acme_order_by_domain(domain_id)
        .await?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Order not found")
                .detail(format!("No ACME order found for domain ID {}", domain_id))
                .build()
        })?;

    // Convert order to response
    let mut response = AcmeOrderResponse::from(order.clone());

    // Fetch live challenge validation status from Let's Encrypt
    if let Ok(Some(challenge_json)) = app_state
        .tls_service
        .get_live_challenge_status(&order.order_url, &order.email)
        .await
    {
        // Convert JSON to ChallengeValidationStatus
        if let Ok(challenge_status) =
            serde_json::from_value::<ChallengeValidationStatus>(challenge_json)
        {
            response.challenge_validation = Some(challenge_status);
        }
    }

    Ok((StatusCode::OK, Json(response)))
}

/// List all ACME orders
#[utoipa::path(
    get,
    path = "/orders",
    responses(
        (status = 200, description = "Orders retrieved successfully", body = ListOrdersResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    params(
        temps_core::PaginationParams,
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_orders(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Query(pagination): Query<temps_core::PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    let (page, page_size) = pagination.normalize();

    info!("Listing all ACME orders for user: {}", auth.user_id());

    let acme_orders = app_state
        .repository
        .list_all_orders_paginated(page, page_size)
        .await
        .map_err(|e| {
            temps_core::problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Failed to list orders")
                .with_detail(e.to_string())
        })?;

    let orders: Vec<AcmeOrderResponse> = acme_orders
        .into_iter()
        .map(|order| AcmeOrderResponse {
            id: order.id,
            order_url: order.order_url,
            domain_id: order.domain_id,
            email: order.email,
            status: order.status,
            identifiers: order.identifiers,
            authorizations: order.authorizations,
            finalize_url: order.finalize_url,
            certificate_url: order.certificate_url,
            error: order.error,
            error_type: order.error_type,
            created_at: order.created_at.timestamp(),
            updated_at: order.updated_at.timestamp(),
            expires_at: order.expires_at.map(|dt| dt.timestamp()),
            challenge_validation: None,
        })
        .collect();

    Ok((StatusCode::OK, Json(ListOrdersResponse { orders })))
}

/// Get HTTP challenge debug information
///
/// Returns detailed debug information for HTTP-01 challenge including:
/// - Whether a challenge exists for the domain
/// - The challenge token and URL that Let's Encrypt will access
/// - DNS resolution information showing where the domain currently points
///
/// This is useful for debugging why HTTP-01 challenges fail.
#[utoipa::path(
    get,
    path = "/domains/{domain}/http-challenge-debug",
    responses(
        (status = 200, description = "Debug information retrieved successfully", body = HttpChallengeDebugResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_http_challenge_debug(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(domain): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    info!(
        "Getting HTTP challenge debug info for domain: {} for user: {}",
        domain,
        auth.user_id()
    );

    let debug_info = app_state
        .tls_service
        .get_http_challenge_debug(&domain)
        .await
        .map_err(|e| {
            error!(
                "Failed to get HTTP challenge debug info for {}: {}",
                domain, e
            );
            e
        })?;

    info!("HTTP challenge debug info retrieved for domain: {}", domain);

    Ok((
        StatusCode::OK,
        Json(HttpChallengeDebugResponse::from(debug_info)),
    ))
}

/// Setup DNS challenge records automatically using a DNS provider
///
/// This endpoint automatically creates the required DNS TXT records for ACME DNS-01 challenge
/// validation using a configured DNS provider. The domain must have an active DNS challenge
/// pending (created via POST /domains/{id}/order with dns-01 challenge type).
///
/// This is similar to how email domain DNS records are auto-provisioned.
#[utoipa::path(
    post,
    path = "/domains/{domain_id}/setup-dns",
    request_body = SetupDnsChallengeRequest,
    responses(
        (status = 200, description = "DNS records created successfully", body = SetupDnsChallengeResponse),
        (status = 400, description = "Bad request - DNS provider not configured or no challenge pending"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain or DNS provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain_id" = i32, Path, description = "Domain ID")
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn setup_dns_challenge(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(domain_id): Path<i32>,
    Json(request): Json<SetupDnsChallengeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsWrite);

    // Check if DNS provider service is available
    let dns_provider_service = app_state.dns_provider_service.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("DNS Provider Service Not Configured")
            .detail("DNS provider service is not configured on this server")
            .build()
    })?;

    // Get the domain
    let domain = app_state
        .domain_service
        .get_domain_by_id(domain_id)
        .await
        .map_err(|e| {
            error!("Failed to get domain by ID {}: {}", domain_id, e);
            e
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("Domain not found")
                .detail(format!("Domain with ID {} not found", domain_id))
                .build()
        })?;

    // Check if this is a DNS-01 challenge
    if domain.verification_method != "dns-01" {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid Challenge Type")
            .detail(format!(
                "Domain {} uses {} challenge type, but DNS auto-provisioning is only available for dns-01 challenges",
                domain.domain, domain.verification_method
            ))
            .build());
    }

    // Get the ACME order with challenge data
    let order = app_state
        .repository
        .find_acme_order_by_domain(domain_id)
        .await?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("No Challenge Pending")
                .detail(format!(
                    "No ACME order found for domain {}. Please create an order first using POST /domains/{}/order",
                    domain.domain, domain_id
                ))
                .build()
        })?;

    // Extract DNS TXT records from the order's authorizations
    let authorizations = order.authorizations.clone().unwrap_or_default();
    let dns_txt_records: Vec<(String, String)> = if let Some(records_json) = authorizations
        .get("dns_txt_records")
        .and_then(|v| v.as_array())
    {
        records_json
            .iter()
            .filter_map(|rec| {
                let name = rec["name"].as_str()?.to_string();
                let value = rec["value"].as_str()?.to_string();
                Some((name, value))
            })
            .collect()
    } else {
        vec![]
    };

    if dns_txt_records.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("No DNS Records Found")
            .detail("No DNS TXT records found in the challenge. The order may not have been created correctly.")
            .build());
    }

    // Get the DNS provider
    let dns_provider = dns_provider_service
        .get(request.dns_provider_id)
        .await
        .map_err(|e| {
            error!("Failed to get DNS provider: {}", e);
            ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("DNS Provider Not Found")
                .detail(format!(
                    "DNS provider with ID {} not found",
                    request.dns_provider_id
                ))
                .build()
        })?;

    // Create DNS provider instance
    let provider_instance = dns_provider_service
        .create_provider_instance(&dns_provider)
        .map_err(|e| {
            error!("Failed to create DNS provider instance: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("DNS Provider Error")
                .detail(format!("Failed to initialize DNS provider: {}", e))
                .build()
        })?;

    // Extract the base domain for the DNS provider
    let base_domain = extract_base_domain(&domain.domain);

    info!(
        "Setting up {} DNS TXT record(s) for {} using provider {}",
        dns_txt_records.len(),
        domain.domain,
        dns_provider.name
    );

    let mut results = Vec::new();
    let mut records_created: u32 = 0;

    // Create each DNS TXT record
    for (name, value) in &dns_txt_records {
        let result =
            create_acme_txt_record(provider_instance.as_ref(), &base_domain, name, value).await;

        if result.success {
            records_created += 1;
        }

        results.push(result);
    }

    let total_records = dns_txt_records.len() as u32;
    let all_success = records_created == total_records;

    let message = if all_success {
        format!(
            "Successfully created all {} DNS TXT record(s) for {} challenge. You can now finalize the order.",
            total_records, domain.domain
        )
    } else {
        format!(
            "Created {} of {} DNS TXT record(s) for {}. Some records may need manual configuration.",
            records_created, total_records, domain.domain
        )
    };

    info!("{}", message);

    let response = SetupDnsChallengeResponse {
        success: all_success,
        records_created,
        total_records,
        results,
        message,
    };

    // Audit log
    let audit = DomainAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        domain: domain.domain.clone(),
        action: "DNS_CHALLENGE_SETUP".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(response))
}

/// Extract the base domain from a full domain name (e.g., "sub.example.com" -> "example.com")
fn extract_base_domain(domain: &str) -> String {
    // Handle wildcard domains
    let domain = domain.strip_prefix("*.").unwrap_or(domain);

    let parts: Vec<&str> = domain.split('.').collect();
    if parts.len() >= 2 {
        parts[parts.len() - 2..].join(".")
    } else {
        domain.to_string()
    }
}

/// Create a single ACME challenge TXT record using the DNS provider
/// This will first remove any existing TXT records with the same name before creating the new one
async fn create_acme_txt_record(
    provider: &dyn temps_dns::providers::DnsProvider,
    base_domain: &str,
    name: &str,
    value: &str,
) -> DnsChallengeRecordResult {
    use temps_dns::providers::{DnsRecordContent, DnsRecordRequest, DnsRecordType};

    // Extract the record name relative to the base domain
    // e.g., "_acme-challenge.example.com" for base domain "example.com" -> "_acme-challenge"
    let record_name = if name.ends_with(&format!(".{}", base_domain)) {
        name.strip_suffix(&format!(".{}", base_domain))
            .unwrap_or(name)
            .to_string()
    } else if name == base_domain {
        "@".to_string()
    } else {
        name.to_string()
    };

    debug!(
        "Creating TXT record: name={} (relative: {}), value={}, base_domain={}",
        name, record_name, value, base_domain
    );

    // First, try to remove any existing TXT record with the same name
    // This is important for ACME challenges as old tokens can interfere with validation
    match provider
        .remove_record(base_domain, &record_name, DnsRecordType::TXT)
        .await
    {
        Ok(()) => {
            debug!(
                "Removed existing TXT record {} for {} before creating new one",
                record_name, base_domain
            );
        }
        Err(e) => {
            // It's okay if removal fails (record might not exist)
            debug!(
                "No existing TXT record to remove for {} (or removal failed: {})",
                record_name, e
            );
        }
    }

    let request = DnsRecordRequest {
        name: record_name.clone(),
        content: DnsRecordContent::TXT {
            content: value.to_string(),
        },
        ttl: Some(120), // Short TTL for ACME challenges
        proxied: false,
    };

    match provider.create_record(base_domain, request).await {
        Ok(_record) => {
            info!(
                "Successfully created TXT record {} = {} for {}",
                name, value, base_domain
            );
            DnsChallengeRecordResult {
                name: name.to_string(),
                value: value.to_string(),
                success: true,
                message: "TXT record created successfully".to_string(),
            }
        }
        Err(e) => {
            error!(
                "Failed to create TXT record {} for {}: {}",
                name, base_domain, e
            );
            DnsChallengeRecordResult {
                name: name.to_string(),
                value: value.to_string(),
                success: false,
                message: format!("Failed to create TXT record: {}", e),
            }
        }
    }
}

pub fn configure_routes() -> Router<Arc<DomainAppState>> {
    Router::new()
        .route("/domains", post(create_domain))
        .route("/domains", get(list_domains))
        .route("/domains/{domain}", get(get_domain_by_id))
        .route("/domains/{domain}/status", get(check_domain_status))
        .route("/domains/by-host/{hostname}", get(get_domain_by_host))
        // Domain-based routes (using domain name)
        .route("/domains/{domain}", delete(delete_domain))
        .route("/domains/{domain}/provision", post(provision_domain))
        .route("/domains/{domain}/renew", post(renew_domain))
        .route("/domains/{domain}/challenge", get(get_domain_challenge))
        .route("/domains/{domain}/dns-completion", get(get_dns_completion))
        .route(
            "/domains/{domain}/challenge-token",
            get(get_challenge_token),
        )
        .route(
            "/domains/{domain}/http-challenge-debug",
            get(get_http_challenge_debug),
        )
        // ACME order management routes (using domain ID)
        .route("/domains/{domain_id}/order", post(create_or_recreate_order))
        .route("/domains/{domain_id}/order", get(get_domain_order))
        .route("/domains/{domain_id}/order", delete(cancel_domain_order))
        .route("/domains/{domain_id}/order/finalize", post(finalize_order))
        // DNS challenge auto-provisioning
        .route("/domains/{domain_id}/setup-dns", post(setup_dns_challenge))
        .route("/orders", get(list_orders))
}
