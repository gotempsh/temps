// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::{
    AcmeOrderResponse, CertStatusResponse, ChallengeError, ChallengeValidationStatus,
    CreateDomainRequest, DnsChallengeRecordResult, DnsCompletionResponse, DomainAppState,
    DomainChallengeResponse, DomainError, DomainResponse, HttpChallengeDebugResponse,
    ListDomainsResponse, ListOnDemandCertsResponse, ListOrdersResponse,
    ListRenewalAttemptsResponse, OnDemandCertAttemptResponse, OnDemandCertRow, ProvisionResponse,
    RenewalAttemptResponse, SetupDnsChallengeRequest, SetupDnsChallengeResponse, TxtRecord,
};
use crate::tls::models::DNS_CLEANUP_PLAN_KEY;
use crate::tls::{ProviderError, RepositoryError, TlsError};
use crate::DomainServiceError;
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::SensitiveAction;
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

/// Exact provider records created by `setup-dns`. Values are retained inside
/// the already-private ACME order so finalization removes only this order's
/// challenges, never a concurrent wildcard/base-domain sibling record that
/// happens to share the same `_acme-challenge` name.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct DnsCleanupPlan {
    provider_id: i32,
    zone: String,
    records: Vec<DnsCleanupRecord>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct DnsCleanupRecord {
    name: String,
    value: String,
    record_id: String,
}

#[derive(Debug)]
struct DnsSetupOutcome {
    results: Vec<DnsChallengeRecordResult>,
    records_created: u32,
    cleanup_records: Vec<DnsCleanupRecord>,
}

#[derive(Debug)]
struct DnsCleanupOutcome {
    deleted: u32,
    errors: Vec<DnsCleanupError>,
    remaining_records: Vec<DnsCleanupRecord>,
}

#[async_trait::async_trait]
trait DnsCleanupReceiptStore: Send + Sync {
    async fn save_cleanup_order(
        &self,
        order: crate::tls::models::AcmeOrder,
    ) -> Result<(), RepositoryError>;
    async fn delete_cleanup_order(&self, order_url: &str) -> Result<(), RepositoryError>;
}

#[async_trait::async_trait]
impl<T> DnsCleanupReceiptStore for T
where
    T: crate::CertificateRepository + Send + Sync + ?Sized,
{
    async fn save_cleanup_order(
        &self,
        order: crate::tls::models::AcmeOrder,
    ) -> Result<(), RepositoryError> {
        self.save_acme_order(order).await.map(|_| ())
    }

    async fn delete_cleanup_order(&self, order_url: &str) -> Result<(), RepositoryError> {
        self.delete_acme_order(order_url).await
    }
}

#[derive(Debug)]
enum DnsCleanupResolution {
    Complete { deleted: u32 },
    Pending { errors: Vec<DnsCleanupError> },
    Manual { reason: DnsCleanupError },
}

#[derive(Debug, thiserror::Error)]
enum DnsCleanupProgressError {
    #[error("failed to serialize remaining DNS cleanup receipt: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to save remaining DNS cleanup receipt: {source}")]
    Save {
        #[source]
        source: RepositoryError,
    },
    #[error("failed to clear completed DNS cleanup receipt: {source}")]
    Delete {
        #[source]
        source: RepositoryError,
    },
}

#[derive(Debug, thiserror::Error)]
enum DnsSetupProgressError {
    #[error("failed to serialize pre-mutation DNS cleanup intent: {source}")]
    IntentSerialize {
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to save pre-mutation DNS cleanup intent: {source}")]
    IntentSave {
        #[source]
        source: RepositoryError,
    },
    #[error(
        "DNS records were changed but exact cleanup receipts could not be serialized: {source}"
    )]
    ReceiptSerialize {
        #[source]
        source: serde_json::Error,
    },
    #[error("DNS records were changed but exact cleanup receipts could not be saved: {source}")]
    ReceiptSave {
        #[source]
        source: RepositoryError,
    },
}

impl DnsSetupProgressError {
    fn records_may_have_changed(&self) -> bool {
        matches!(
            self,
            Self::ReceiptSerialize { .. } | Self::ReceiptSave { .. }
        )
    }
}

#[derive(Debug, thiserror::Error)]
enum DnsCleanupError {
    #[error(
        "DNS provider '{provider}' represents TXT values as a shared record set; automatic ACME cleanup was skipped to preserve concurrent challenge values"
    )]
    SharedRecordSet {
        provider: temps_dns::providers::DnsProviderType,
    },
    #[error(
        "DNS cleanup receipt for TXT record '{name}' in zone '{zone}' has no provider record ID; automatic deletion is unsafe"
    )]
    MissingRecordId { zone: String, name: String },
    #[error(
        "Failed to delete ACME TXT record '{name}' (provider record {record_id}) in DNS zone '{zone}': {source}"
    )]
    DeleteRecord {
        zone: String,
        name: String,
        record_id: String,
        #[source]
        source: Box<temps_dns::errors::DnsError>,
    },
}

impl AuditOperation for DomainAudit {
    fn operation_type(&self) -> String {
        self.action.clone()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
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
            DomainServiceError::OnDemandRateLimited {
                hostname, detail, ..
            } => ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
                .title("Let's Encrypt Rate Limit Reached")
                .detail(format!(
                    "On-demand TLS for {hostname} was rate limited by Let's Encrypt: {detail}"
                ))
                .build(),
            DomainServiceError::OnDemandIssuanceFailed {
                hostname,
                category,
                error_chain,
            } => ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                .title("On-demand TLS Issuance Failed")
                .detail(format!(
                    "On-demand TLS issuance for {hostname} failed ({category}): {error_chain}"
                ))
                .build(),
            DomainServiceError::CertificateAlreadyActive(hostname) => {
                // This is a no-op success path, not an error that should reach
                // HTTP handlers. Map to 200 OK conceptually, but if it somehow
                // surfaces here, return 409 Conflict as a safe fallback.
                ErrorBuilder::new(StatusCode::CONFLICT)
                    .title("Certificate Already Active")
                    .detail(format!(
                        "Domain {hostname} already has an active certificate"
                    ))
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
        list_renewal_attempts,
        get_challenge_token,
        create_or_recreate_order,
        cancel_domain_order,
        get_domain_order,
        list_orders,
        get_http_challenge_debug,
        setup_dns_challenge,
        list_on_demand_certs,
        get_on_demand_cert_status
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
            DnsChallengeRecordResult,
            ListOnDemandCertsResponse,
            OnDemandCertRow,
            OnDemandCertAttemptResponse,
            CertStatusResponse,
            ListRenewalAttemptsResponse,
            RenewalAttemptResponse
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
        (status = 400, description = "Bad request - account email or challenge is invalid"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Domain permission denied or a user account is required"),
        (status = 404, description = "Domain not found"),
        (status = 409, description = "DNS cleanup-aware order must use the finalize endpoint"),
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
        if let Some(order) = app_state
            .repository
            .find_acme_order_by_domain(domain_model.id)
            .await?
        {
            match dns_provision_requires_cleanup_aware_finalize(&order) {
                Ok(true) => {
                    return Err(ErrorBuilder::new(StatusCode::CONFLICT)
                        .title("DNS Cleanup-Aware Finalization Required")
                        .detail(format!(
                            "Domain {} has automated DNS cleanup receipts. Finalize it with `temps domain order finalize --domain-id {}` so certificate issuance and DNS cleanup complete together.",
                            domain, domain_model.id
                        ))
                        .build());
                }
                Ok(false) => {}
                Err(source) => {
                    return Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Invalid DNS Cleanup Metadata")
                        .detail(format!(
                            "Cannot read persisted DNS cleanup metadata for domain {} (ID {}): {}",
                            domain, domain_model.id, source
                        ))
                        .build());
                }
            }
        }
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
        (status = 400, description = "Bad request - account email or ACME order is invalid"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Domain or DNS provider permission denied"),
        (status = 404, description = "Domain or order not found"),
        (status = 409, description = "Certificate issued but DNS cleanup requires operator action"),
        (status = 502, description = "Certificate issued but DNS provider cleanup failed"),
        (status = 503, description = "Certificate issued but DNS provider service is unavailable"),
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
    let cleanup_order = if domain_model.verification_method == "dns-01" {
        match app_state
            .repository
            .find_acme_order_by_domain(domain_id)
            .await?
        {
            Some(order) => match load_dns_cleanup_plan(&order) {
                Ok(Some(plan)) => Some((order, plan)),
                Ok(None) => None,
                Err(source) => {
                    return Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Invalid DNS Cleanup Metadata")
                        .detail(format!(
                            "Cannot read persisted DNS cleanup metadata for domain {} (ID {}): {}",
                            domain_name, domain_id, source
                        ))
                        .build())
                }
            },
            None => None,
        }
    } else {
        None
    };
    // Finalization normally needs only domain access. A persisted cleanup plan
    // turns it into an external DNS mutation, so require the same DNS provider
    // permission that setup-dns required when the plan was created.
    ensure_dns_cleanup_permission(&auth, cleanup_order.is_some())?;
    info!(
        "Finalizing order for domain: {} (ID: {}) for user: {}",
        domain_name, domain_id, user_email
    );

    // A previous call may have issued the certificate but left the cleanup
    // receipt for retry after a transient provider failure. Do not submit the
    // already-finalized ACME order again in that case.
    let domain =
        if should_retry_dns_cleanup_without_issuance(&domain_model.status, cleanup_order.is_some())
        {
            info!(
                "Retrying pending ACME DNS cleanup for already-active domain {} (ID {})",
                domain_name, domain_id
            );
            domain_model
        } else {
            app_state
                .domain_service
                .complete_challenge(&domain_name, user_email)
                .await
                .map_err(|e| {
                    error!("Failed to finalize order for domain {}: {}", domain_name, e);
                    e
                })?
        };

    info!("Order finalized successfully for domain: {}", domain.domain);

    let mut cleanup_problem = None;
    let mut cleanup_audit_action = "DOMAIN_ORDER_FINALIZED";
    if let Some((order, plan)) = cleanup_order {
        match app_state.dns_provider_service.as_ref() {
            Some(dns_provider_service) => match dns_provider_service.get(plan.provider_id).await {
                Ok(provider) if provider.is_active => {
                    match dns_provider_service.create_provider_instance(&provider) {
                        Ok(provider_instance) => {
                            match execute_dns_cleanup(
                                provider_instance.as_ref(),
                                app_state.repository.as_ref(),
                                order,
                                plan,
                            )
                            .await
                            {
                                Ok(DnsCleanupResolution::Complete { deleted }) => {
                                    cleanup_audit_action =
                                        "DOMAIN_ORDER_FINALIZED_DNS_CLEANUP_COMPLETE";
                                    info!(
                                        "Removed {} ACME DNS TXT record(s) for domain {} after certificate issuance",
                                        deleted, domain_name
                                    );
                                }
                                Ok(DnsCleanupResolution::Pending { errors }) => {
                                    for cleanup_error in errors {
                                        warn!(
                                            "ACME DNS cleanup was incomplete for domain {} (ID {}): {}",
                                            domain_name, domain_id, cleanup_error
                                        );
                                    }
                                    cleanup_problem = Some(
                                        ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                                            .title("Certificate Issued; DNS Cleanup Pending")
                                            .detail(format!(
                                                "The certificate for domain {} was issued, but one or more ACME TXT records could not be removed. Retry finalization to complete cleanup.",
                                                domain_name
                                            ))
                                            .build(),
                                    );
                                    cleanup_audit_action =
                                        "DOMAIN_ORDER_FINALIZED_DNS_CLEANUP_PENDING";
                                }
                                Ok(DnsCleanupResolution::Manual {
                                    reason: DnsCleanupError::SharedRecordSet { provider },
                                }) => {
                                    warn!(
                                        "Automatic ACME DNS cleanup is unsafe for provider {} on domain {} (ID {}); manual cleanup is required",
                                        provider, domain_name, domain_id
                                    );
                                    cleanup_problem = Some(
                                        ErrorBuilder::new(StatusCode::CONFLICT)
                                            .title("Certificate Issued; Manual DNS Cleanup Required")
                                            .detail(format!(
                                                "The certificate for domain {} was issued, but provider {} cannot safely delete one TXT value from a shared record set. Inspect this order's DNS challenge values, remove them manually, then cancel the order to clear its retained cleanup receipt.",
                                                domain_name, provider
                                            ))
                                            .build(),
                                    );
                                    cleanup_audit_action =
                                        "DOMAIN_ORDER_FINALIZED_MANUAL_DNS_CLEANUP";
                                }
                                Ok(DnsCleanupResolution::Manual {
                                    reason: DnsCleanupError::MissingRecordId { zone, name },
                                }) => {
                                    warn!(
                                        "Automatic ACME DNS cleanup lacks a provider record ID for {} in zone {} on domain {} (ID {}); manual cleanup is required",
                                        name, zone, domain_name, domain_id
                                    );
                                    cleanup_problem = Some(
                                        ErrorBuilder::new(StatusCode::CONFLICT)
                                            .title("Certificate Issued; Manual DNS Cleanup Required")
                                            .detail(format!(
                                                "The certificate for domain {} was issued, but an exact provider record ID was not available. Inspect this order's DNS challenge values, remove them manually, then cancel the order to clear its retained cleanup receipt.",
                                                domain_name
                                            ))
                                            .build(),
                                    );
                                    cleanup_audit_action =
                                        "DOMAIN_ORDER_FINALIZED_MANUAL_DNS_CLEANUP";
                                }
                                Ok(DnsCleanupResolution::Manual { reason }) => {
                                    warn!(
                                        "ACME DNS cleanup failed for domain {} (ID {}): {}",
                                        domain_name, domain_id, reason
                                    );
                                    cleanup_problem = Some(
                                        ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                                            .title("Certificate Issued; DNS Cleanup Pending")
                                            .detail(format!(
                                                "The certificate for domain {} was issued, but ACME TXT cleanup failed. Retry finalization to complete cleanup.",
                                                domain_name
                                            ))
                                            .build(),
                                    );
                                    cleanup_audit_action =
                                        "DOMAIN_ORDER_FINALIZED_DNS_CLEANUP_PENDING";
                                }
                                Err(source) => {
                                    warn!(
                                        "DNS cleanup progress could not be persisted for domain {} (ID {}): {}",
                                        domain_name, domain_id, source
                                    );
                                    cleanup_problem = Some(
                                        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                                            .title("Certificate Issued; DNS Cleanup Progress Pending")
                                            .detail(format!(
                                                "The certificate for domain {} was issued, but cleanup progress could not be saved. Retry finalization; already-absent provider records are handled safely.",
                                                domain_name
                                            ))
                                            .build(),
                                    );
                                    cleanup_audit_action =
                                        "DOMAIN_ORDER_FINALIZED_DNS_CLEANUP_PROGRESS_PENDING";
                                }
                            }
                        }
                        Err(source) => {
                            warn!(
                                "Could not initialize DNS provider {} to clean ACME records for domain {} (ID {}): {}",
                                plan.provider_id, domain_name, domain_id, source
                            );
                            cleanup_problem = Some(
                                ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                                    .title("Certificate Issued; DNS Cleanup Pending")
                                    .detail(format!(
                                        "The certificate for domain {} was issued, but its DNS provider could not be initialized. Retry finalization to complete cleanup.",
                                        domain_name
                                    ))
                                    .build(),
                            );
                            cleanup_audit_action = "DOMAIN_ORDER_FINALIZED_DNS_CLEANUP_PENDING";
                        }
                    }
                }
                Ok(_) => {
                    warn!(
                        "DNS provider {} is inactive; ACME TXT records for domain {} (ID {}) were not removed",
                        plan.provider_id, domain_name, domain_id
                    );
                    cleanup_problem = Some(
                        ErrorBuilder::new(StatusCode::CONFLICT)
                            .title("Certificate Issued; DNS Cleanup Pending")
                            .detail(format!(
                                "The certificate for domain {} was issued, but DNS provider {} is inactive. Reactivate it and retry finalization to complete cleanup.",
                                domain_name, plan.provider_id
                            ))
                            .build(),
                    );
                    cleanup_audit_action = "DOMAIN_ORDER_FINALIZED_DNS_CLEANUP_PENDING";
                }
                Err(source) => {
                    warn!(
                        "Could not load DNS provider {} to clean ACME records for domain {} (ID {}): {}",
                        plan.provider_id, domain_name, domain_id, source
                    );
                    cleanup_problem = Some(
                        ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                            .title("Certificate Issued; DNS Cleanup Pending")
                            .detail(format!(
                                "The certificate for domain {} was issued, but DNS provider {} could not be loaded. Retry finalization to complete cleanup.",
                                domain_name, plan.provider_id
                            ))
                            .build(),
                    );
                    cleanup_audit_action = "DOMAIN_ORDER_FINALIZED_DNS_CLEANUP_PENDING";
                }
            },
            None => {
                warn!(
                    "DNS provider service is unavailable; ACME TXT records for domain {} (ID {}) were not removed",
                    domain_name, domain_id
                );
                cleanup_problem = Some(
                    ErrorBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
                        .title("Certificate Issued; DNS Cleanup Pending")
                        .detail(format!(
                            "The certificate for domain {} was issued, but the DNS provider service is unavailable. Retry finalization to complete cleanup.",
                            domain_name
                        ))
                        .build(),
                );
                cleanup_audit_action = "DOMAIN_ORDER_FINALIZED_DNS_CLEANUP_PENDING";
            }
        }
    }

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
        action: cleanup_audit_action.to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    if let Some(problem) = cleanup_problem {
        return Err(problem);
    }

    Ok((StatusCode::OK, Json(DomainResponse::from(domain))))
}

fn should_retry_dns_cleanup_without_issuance(
    domain_status: &str,
    has_cleanup_receipt: bool,
) -> bool {
    has_cleanup_receipt && domain_status == "active"
}

fn dns_provision_requires_cleanup_aware_finalize(
    order: &crate::tls::models::AcmeOrder,
) -> Result<bool, serde_json::Error> {
    load_dns_cleanup_plan(order).map(|plan| plan.is_some())
}

fn ensure_dns_cleanup_permission(
    auth: &temps_auth::AuthContext,
    cleanup_required: bool,
) -> Result<(), Problem> {
    if cleanup_required {
        permission_guard!(auth, DnsProvidersWrite);
    }
    Ok(())
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
    require_sensitive_action(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::DeleteDomain {
            domain: domain.clone(),
        },
    )
    .await?;

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

/// Query parameters for listing renewal attempts.
#[derive(Debug, Clone, serde::Deserialize, utoipa::IntoParams)]
pub struct ListRenewalAttemptsParams {
    /// Page number (1-indexed)
    #[param(example = 1)]
    pub page: Option<u64>,
    /// Number of items per page (max 100)
    #[param(example = 20)]
    pub page_size: Option<u64>,
}

impl ListRenewalAttemptsParams {
    pub fn normalize(&self) -> (u64, u64) {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 100);
        (page, page_size)
    }
}

/// List certificate renewal attempts for a domain
///
/// Returns rows from the append-only `renewal_attempts` audit log, newest
/// first: every `request_challenge` (order creation) and `complete_challenge`
/// (order finalization) attempt for this domain, successful or failed, with
/// the full error detail. Backs the domain detail page's renewal timeline —
/// `domains.last_error` only ever holds the MOST RECENT failure, so this is
/// the only way to see the history behind it.
#[utoipa::path(
    get,
    path = "/domains/{domain}/renewal-attempts",
    responses(
        (status = 200, description = "Renewal attempts retrieved successfully", body = ListRenewalAttemptsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Domain not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("domain" = String, Path, description = "Domain name"),
        ListRenewalAttemptsParams,
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_renewal_attempts(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(domain): Path<String>,
    Query(params): Query<ListRenewalAttemptsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    let (page, page_size) = params.normalize();

    debug!(
        "Listing renewal attempts for domain {} (page={}, page_size={}) for user: {}",
        domain,
        page,
        page_size,
        auth.user_id()
    );

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

    let (rows, total) = app_state
        .domain_service
        .list_renewal_attempts(domain_model.id, page, page_size)
        .await
        .map_err(|e| {
            error!(
                "Failed to list renewal attempts for domain {}: {}",
                domain, e
            );
            e
        })?;

    let attempts: Vec<RenewalAttemptResponse> =
        rows.into_iter().map(RenewalAttemptResponse::from).collect();

    Ok((
        StatusCode::OK,
        Json(ListRenewalAttemptsResponse {
            attempts,
            total,
            page,
            page_size,
        }),
    ))
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
        .map(AcmeOrderResponse::from)
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
        (status = 409, description = "Ambiguous managed DNS zone"),
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
    permission_guard!(auth, DnsProvidersWrite);

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

    // This governance check intentionally precedes zone lookup and provider-client
    // construction, which decrypts credentials and can initiate external calls.
    ensure_dns_provider_active(&dns_provider, &domain.domain, domain_id)?;

    let managed_domain = dns_provider_service
        .find_verified_zone_for_provider(request.dns_provider_id, &domain.domain)
        .await
        .map_err(|e| {
            error!(
                "Failed to find a verified DNS zone for domain {} and provider {}: {}",
                domain.domain, request.dns_provider_id, e
            );
            match e {
                temps_dns::errors::DnsError::AmbiguousManagedDomain { .. } => {
                    ErrorBuilder::new(StatusCode::CONFLICT)
                        .title("Ambiguous Managed DNS Zone")
                        .detail(format!(
                            "Multiple verified managed DNS zones match domain {} for provider {}. Remove the duplicate managed-domain entries and retry.",
                            domain.domain, request.dns_provider_id
                        ))
                        .build()
                }
                _ => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title("DNS Zone Lookup Failed")
                    .detail(format!(
                        "Failed to verify that DNS provider {} manages domain {}",
                        request.dns_provider_id, domain.domain
                    ))
                    .build(),
            }
        })?
        .ok_or_else(|| {
            ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("DNS Provider Does Not Manage Domain")
                .detail(format!(
                    "DNS provider {} has no verified zone covering domain {}",
                    request.dns_provider_id, domain.domain
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

    let authoritative_zone = managed_domain.domain;

    info!(
        "Setting up {} DNS TXT record(s) for {} using provider {}",
        dns_txt_records.len(),
        domain.domain,
        dns_provider.name
    );

    let setup_outcome = match execute_dns_setup(
        provider_instance.as_ref(),
        app_state.repository.as_ref(),
        order,
        request.dns_provider_id,
        &authoritative_zone,
        &dns_txt_records,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(source) if source.records_may_have_changed() => {
            error!(
                "DNS records may have changed for domain {} (ID {}), but exact cleanup receipts could not be saved: {}",
                domain.domain, domain_id, source
            );
            let audit = DomainAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
                domain: domain.domain.clone(),
                action: "DNS_CHALLENGE_SETUP_RECEIPT_UPDATE_FAILED".to_string(),
            };
            if let Err(audit_error) = app_state.audit_service.create_audit_log(&audit).await {
                error!(
                    "Failed to create DNS setup failure audit log: {}",
                    audit_error
                );
            }
            return Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("DNS Records Created; Cleanup Receipt Update Failed")
                .detail(format!(
                    "DNS challenge records may have been created for domain {} (ID {}), but their exact provider IDs could not be saved. The original cleanup intent was retained; inspect the order and DNS provider before retrying.",
                    domain.domain, domain_id
                ))
                .build());
        }
        Err(source) => {
            error!(
                "DNS cleanup intent could not be persisted for domain {} (ID {}): {}",
                domain.domain, domain_id, source
            );
            return Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("DNS Cleanup Intent Persistence Failed")
                .detail(format!(
                    "DNS records were not changed because cleanup intent could not be saved for domain {} (ID {}).",
                    domain.domain, domain_id
                ))
                .build());
        }
    };
    let DnsSetupOutcome {
        results,
        records_created,
        ..
    } = setup_outcome;

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

fn ensure_dns_provider_active(
    provider: &temps_entities::dns_providers::Model,
    domain: &str,
    domain_id: i32,
) -> Result<(), Problem> {
    if provider.is_active {
        return Ok(());
    }

    Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
        .title("DNS Provider Is Inactive")
        .detail(format!(
            "DNS provider {} ({}) is inactive and cannot set up the DNS challenge for domain {} (ID {})",
            provider.id, provider.name, domain, domain_id
        ))
        .build())
}

/// Extract the record name relative to the base domain
/// e.g., "_acme-challenge.example.com" for base domain "example.com" -> "_acme-challenge"
fn acme_txt_record_name(base_domain: &str, name: &str) -> String {
    let base_domain = base_domain.trim_end_matches('.');
    let name = name.trim_end_matches('.');
    if name.ends_with(&format!(".{}", base_domain)) {
        name.strip_suffix(&format!(".{}", base_domain))
            .unwrap_or(name)
            .to_string()
    } else if name == base_domain {
        "@".to_string()
    } else {
        name.to_string()
    }
}

fn dns_cleanup_plan(
    provider_id: i32,
    zone: &str,
    cleanup_records: Vec<DnsCleanupRecord>,
) -> DnsCleanupPlan {
    let mut seen = std::collections::HashSet::new();
    DnsCleanupPlan {
        provider_id,
        zone: zone.trim_end_matches('.').to_string(),
        records: cleanup_records
            .into_iter()
            .filter(|record| {
                seen.insert((
                    record.record_id.clone(),
                    record.name.clone(),
                    record.value.clone(),
                ))
            })
            .collect(),
    }
}

fn store_dns_cleanup_plan(
    order: &mut crate::tls::models::AcmeOrder,
    plan: &DnsCleanupPlan,
) -> Result<(), serde_json::Error> {
    let authorizations = order
        .authorizations
        .get_or_insert_with(|| serde_json::json!({}));
    let Some(object) = authorizations.as_object_mut() else {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            "ACME order authorizations must be a JSON object",
        ));
    };
    object.insert(
        DNS_CLEANUP_PLAN_KEY.to_string(),
        serde_json::to_value(plan)?,
    );
    Ok(())
}

fn load_dns_cleanup_plan(
    order: &crate::tls::models::AcmeOrder,
) -> Result<Option<DnsCleanupPlan>, serde_json::Error> {
    order
        .authorizations
        .as_ref()
        .and_then(|authorizations| authorizations.get(DNS_CLEANUP_PLAN_KEY))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
}

/// Remove the exact provider record IDs returned when this ACME order created
/// its TXT values. Providers can have multiple values at the same name during
/// wildcard issuance, so a later name-only lookup is unsafe and can also miss
/// records beyond a provider's first list page.
async fn cleanup_dns_txt_records(
    provider: &dyn temps_dns::providers::DnsProvider,
    plan: &DnsCleanupPlan,
) -> Result<DnsCleanupOutcome, DnsCleanupError> {
    // Cloudflare and DigitalOcean expose a distinct ID for each TXT value.
    // Pebble is the explicitly gated, local-test-only exception: its synthetic
    // ID is the challenge FQDN and delete_record calls challtestsrv's
    // post-validation clear-txt endpoint. Azure, GCP, Route53, and Namecheap
    // expose one synthetic ID per RRset/name in real user-controlled zones, so
    // calling their delete_record implementation could remove a sibling ACME
    // order's value even after our exact-value filter succeeds.
    let provider_type = provider.provider_type();
    if !matches!(
        provider_type,
        temps_dns::providers::DnsProviderType::Cloudflare
            | temps_dns::providers::DnsProviderType::DigitalOcean
            | temps_dns::providers::DnsProviderType::Pebble
    ) {
        return Err(DnsCleanupError::SharedRecordSet {
            provider: provider_type,
        });
    }

    let mut deleted = 0;
    let mut errors = Vec::new();
    let mut remaining_records = Vec::new();
    for expected in &plan.records {
        if expected.record_id.is_empty() {
            return Err(DnsCleanupError::MissingRecordId {
                zone: plan.zone.clone(),
                name: expected.name.clone(),
            });
        }
        match provider
            .delete_record(&plan.zone, &expected.record_id)
            .await
        {
            Ok(()) => deleted += 1,
            // Deletion is idempotent: the provider may have applied a previous
            // request whose response or subsequent receipt write was lost.
            Err(temps_dns::errors::DnsError::RecordNotFound(_)) => deleted += 1,
            Err(source) => {
                remaining_records.push(expected.clone());
                errors.push(DnsCleanupError::DeleteRecord {
                    zone: plan.zone.clone(),
                    name: expected.name.clone(),
                    record_id: expected.record_id.clone(),
                    source: Box::new(source),
                });
            }
        }
    }

    Ok(DnsCleanupOutcome {
        deleted,
        errors,
        remaining_records,
    })
}

async fn execute_dns_setup<S>(
    provider: &dyn temps_dns::providers::DnsProvider,
    receipt_store: &S,
    mut order: crate::tls::models::AcmeOrder,
    provider_id: i32,
    zone: &str,
    dns_txt_records: &[(String, String)],
) -> Result<DnsSetupOutcome, DnsSetupProgressError>
where
    S: DnsCleanupReceiptStore + ?Sized,
{
    let pending_records = dns_txt_records
        .iter()
        .map(|(name, value)| DnsCleanupRecord {
            name: acme_txt_record_name(zone, name),
            value: value.clone(),
            record_id: String::new(),
        })
        .collect();
    let pending_plan = dns_cleanup_plan(provider_id, zone, pending_records);
    store_dns_cleanup_plan(&mut order, &pending_plan)
        .map_err(|source| DnsSetupProgressError::IntentSerialize { source })?;
    receipt_store
        .save_cleanup_order(order.clone())
        .await
        .map_err(|source| DnsSetupProgressError::IntentSave { source })?;

    let outcome = setup_dns_txt_records_with_cleanup(provider, zone, dns_txt_records).await;
    let exact_plan = dns_cleanup_plan(provider_id, zone, outcome.cleanup_records.clone());
    store_dns_cleanup_plan(&mut order, &exact_plan)
        .map_err(|source| DnsSetupProgressError::ReceiptSerialize { source })?;
    receipt_store
        .save_cleanup_order(order)
        .await
        .map_err(|source| DnsSetupProgressError::ReceiptSave { source })?;
    Ok(outcome)
}

async fn execute_dns_cleanup<S>(
    provider: &dyn temps_dns::providers::DnsProvider,
    receipt_store: &S,
    mut order: crate::tls::models::AcmeOrder,
    mut plan: DnsCleanupPlan,
) -> Result<DnsCleanupResolution, DnsCleanupProgressError>
where
    S: DnsCleanupReceiptStore + ?Sized,
{
    let outcome = match cleanup_dns_txt_records(provider, &plan).await {
        Ok(outcome) => outcome,
        Err(reason) => return Ok(DnsCleanupResolution::Manual { reason }),
    };

    if outcome.errors.is_empty() {
        receipt_store
            .delete_cleanup_order(&order.order_url)
            .await
            .map_err(|source| DnsCleanupProgressError::Delete { source })?;
        return Ok(DnsCleanupResolution::Complete {
            deleted: outcome.deleted,
        });
    }

    plan.records = outcome.remaining_records;
    store_dns_cleanup_plan(&mut order, &plan)
        .map_err(|source| DnsCleanupProgressError::Serialize { source })?;
    receipt_store
        .save_cleanup_order(order)
        .await
        .map_err(|source| DnsCleanupProgressError::Save { source })?;
    Ok(DnsCleanupResolution::Pending {
        errors: outcome.errors,
    })
}

/// Remove stale TXT records left over from a previous order/renewal, then create every
/// record in the batch. Cleanup happens once per distinct record name, before ANY record
/// in the batch is created: a wildcard order publishes two TXT records under the same
/// `_acme-challenge` name (one per authorization), so removing per-record (interleaved
/// with creation) would delete a sibling record this same batch just created.
pub(crate) async fn setup_dns_txt_records(
    provider: &dyn temps_dns::providers::DnsProvider,
    base_domain: &str,
    dns_txt_records: &[(String, String)],
) -> (Vec<DnsChallengeRecordResult>, u32) {
    let outcome = setup_dns_txt_records_with_cleanup(provider, base_domain, dns_txt_records).await;
    (outcome.results, outcome.records_created)
}

async fn setup_dns_txt_records_with_cleanup(
    provider: &dyn temps_dns::providers::DnsProvider,
    base_domain: &str,
    dns_txt_records: &[(String, String)],
) -> DnsSetupOutcome {
    use std::collections::HashSet;
    use temps_dns::providers::DnsRecordType;

    let mut cleaned_names = HashSet::new();
    for (name, _value) in dns_txt_records {
        let record_name = acme_txt_record_name(base_domain, name);
        if cleaned_names.insert(record_name.clone()) {
            if let Err(e) = provider
                .remove_record(base_domain, &record_name, DnsRecordType::TXT)
                .await
            {
                debug!(
                    "No existing TXT record to remove for {} (or removal failed: {})",
                    record_name, e
                );
            }
        }
    }

    let mut results = Vec::new();
    let mut records_created: u32 = 0;
    let mut cleanup_records = Vec::new();
    for (name, value) in dns_txt_records {
        let (result, cleanup_record) =
            create_acme_txt_record(provider, base_domain, name, value).await;
        if result.success {
            records_created += 1;
        }
        if let Some(cleanup_record) = cleanup_record {
            cleanup_records.push(cleanup_record);
        }
        results.push(result);
    }

    DnsSetupOutcome {
        results,
        records_created,
        cleanup_records,
    }
}

pub(crate) enum DnsAutomationAuthorization {
    Allowed,
    Denied(String),
    AuthorizationError(String),
}

/// Validate and authorize an unattended DNS mutation before callers construct
/// a provider client. This function is deliberately provider-independent so a
/// denied or failed decision cannot decrypt provider credentials.
pub(crate) async fn authorize_dns_automation_request(
    gate: &dyn temps_core::DnsAutomationGate,
    request: &temps_core::DnsAutomationRequest,
    actual_provider_id: i32,
) -> DnsAutomationAuthorization {
    if let Err(reason) = validate_dns_automation_request(request, actual_provider_id) {
        return DnsAutomationAuthorization::Denied(reason);
    }
    match gate.authorize(request).await {
        Ok(temps_core::DnsAutomationDecision::Allow) => DnsAutomationAuthorization::Allowed,
        // Policy implementations receive the ACME proof in `request`. Their
        // free-form reason must never cross into logs or durable audit data,
        // because a buggy implementation could reflect that proof verbatim.
        Ok(temps_core::DnsAutomationDecision::Deny { .. }) => {
            DnsAutomationAuthorization::Denied("automation policy denied the request".to_string())
        }
        Err(_) => DnsAutomationAuthorization::AuthorizationError(
            "automation policy evaluation failed".to_string(),
        ),
    }
}

fn normalize_dns_name(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

pub(crate) fn validate_dns_automation_request(
    request: &temps_core::DnsAutomationRequest,
    actual_provider_id: i32,
) -> Result<(), String> {
    if request.purpose != temps_core::DnsAutomationPurpose::AcmeDns01 {
        return Err("background DNS mutation boundary accepts only ACME DNS-01 requests".into());
    }
    if request.provider_id != actual_provider_id {
        return Err("authorized DNS provider does not match the provider instance".into());
    }
    if request.mutations.is_empty() {
        return Err("background DNS mutation batch must not be empty".into());
    }

    let zone = normalize_dns_name(&request.zone);
    let domain = normalize_dns_name(request.domain.trim_start_matches("*."));
    if zone.is_empty()
        || domain.is_empty()
        || (domain != zone && !domain.ends_with(&format!(".{zone}")))
    {
        return Err("request domain is not covered by the authoritative DNS zone".into());
    }

    let expected_name = format!("_acme-challenge.{domain}");
    for mutation in &request.mutations {
        if !mutation.record_type.eq_ignore_ascii_case("TXT") {
            return Err("background DNS mutation boundary accepts only TXT records".into());
        }
        if mutation.value.trim().is_empty() {
            return Err("ACME DNS-01 mutation values must not be empty".into());
        }
        if normalize_dns_name(&mutation.name) != expected_name {
            return Err(format!(
                "DNS mutation name must be exactly {expected_name} for this ACME authorization"
            ));
        }
    }

    Ok(())
}

/// Create a single ACME challenge TXT record using the DNS provider.
/// Callers must remove stale records for every name in the batch before calling this
/// (see `setup_dns_txt_records`) -- removing here, per-record, would delete a sibling
/// authorization's record that a wildcard order just created under the same name.
async fn create_acme_txt_record(
    provider: &dyn temps_dns::providers::DnsProvider,
    base_domain: &str,
    name: &str,
    value: &str,
) -> (DnsChallengeRecordResult, Option<DnsCleanupRecord>) {
    use temps_dns::providers::{DnsRecordContent, DnsRecordRequest};

    let record_name = acme_txt_record_name(base_domain, name);

    debug!(
        "Creating TXT record: name={} (relative: {}), base_domain={}",
        name, record_name, base_domain
    );

    let request = DnsRecordRequest {
        name: record_name.clone(),
        content: DnsRecordContent::TXT {
            content: value.to_string(),
        },
        ttl: Some(120), // Short TTL for ACME challenges
        proxied: false,
    };

    match provider.create_record(base_domain, request).await {
        Ok(record) => {
            info!(
                "Successfully created TXT record {} for {}",
                name, base_domain
            );
            let cleanup_record = record.id.map(|record_id| DnsCleanupRecord {
                name: record_name,
                value: value.to_string(),
                record_id,
            });
            (
                DnsChallengeRecordResult {
                    name: name.to_string(),
                    value: value.to_string(),
                    success: true,
                    message: "TXT record created successfully".to_string(),
                },
                cleanup_record,
            )
        }
        Err(e) => {
            error!(
                "Failed to create TXT record {} for {}: {}",
                name, base_domain, e
            );
            (
                DnsChallengeRecordResult {
                    name: name.to_string(),
                    value: value.to_string(),
                    success: false,
                    message: format!("Failed to create TXT record: {}", e),
                },
                None,
            )
        }
    }
}

/// Query parameters for listing on-demand cert attempts.
#[derive(Debug, Clone, serde::Deserialize, utoipa::IntoParams)]
pub struct ListOnDemandCertsParams {
    /// Page number (1-indexed)
    #[param(example = 1)]
    pub page: Option<u64>,
    /// Number of items per page (max 100)
    #[param(example = 20)]
    pub page_size: Option<u64>,
}

impl ListOnDemandCertsParams {
    pub fn normalize(&self) -> (u64, u64) {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 100);
        (page, page_size)
    }
}

/// List on-demand TLS certificate attempts
///
/// Returns rows from the append-only `on_demand_cert_attempts` audit log
/// (ADR-018 §5), newest first, each joined with the current authoritative cert
/// state (`status`, `expiration_time`, `backoff_until`) from the `domains` row.
/// This backs the console "Certificates" surface. No certificate or private-key
/// material is returned — only audit metadata.
#[utoipa::path(
    get,
    path = "/domains/on-demand-certs",
    responses(
        (status = 200, description = "On-demand cert attempts retrieved successfully", body = ListOnDemandCertsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ListOnDemandCertsParams,
    ),
    tag = "Domains",
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_on_demand_certs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Query(params): Query<ListOnDemandCertsParams>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    let (page, page_size) = params.normalize();

    debug!(
        "Listing on-demand cert attempts (page={}, page_size={}) for user: {}",
        page,
        page_size,
        auth.user_id()
    );

    let (rows, total) = app_state
        .domain_service
        .list_on_demand_attempts(page, page_size)
        .await
        .map_err(|e| {
            error!("Failed to list on-demand cert attempts: {}", e);
            e
        })?;

    let certs: Vec<OnDemandCertRow> = rows
        .into_iter()
        .map(|row| {
            let (status, expiration_time, backoff_until) = match row.domain {
                Some(d) => (
                    Some(d.status),
                    d.expiration_time.map(|dt| dt.timestamp_millis()),
                    d.on_demand_backoff_until.map(|dt| dt.timestamp_millis()),
                ),
                None => (None, None, None),
            };
            OnDemandCertRow {
                hostname: row.attempt.hostname.clone(),
                status,
                expiration_time,
                backoff_until,
                attempt: OnDemandCertAttemptResponse::from(row.attempt),
            }
        })
        .collect();

    Ok((
        StatusCode::OK,
        Json(ListOnDemandCertsResponse {
            certs,
            total,
            page,
            page_size,
        }),
    ))
}

/// Get on-demand TLS certificate status for a hostname
///
/// Returns the current cert lifecycle state for a single hostname (from the
/// `domains` row) plus the most recent on-demand issuance attempt (from the
/// `on_demand_cert_attempts` audit log). This is the operator's first-line
/// diagnostic, surfaced by `temps domain cert-status` (ADR-018 §5). Returns the
/// hostname with `None` fields when no on-demand activity exists for it (never a
/// 404, so the CLI can render "no attempts recorded").
#[utoipa::path(
    get,
    path = "/domains/by-host/{hostname}/cert-status",
    responses(
        (status = 200, description = "On-demand cert status retrieved successfully", body = CertStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
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
async fn get_on_demand_cert_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<DomainAppState>>,
    Path(hostname): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);

    debug!(
        "Getting on-demand cert status for hostname: {} (user: {})",
        hostname,
        auth.user_id()
    );

    let status = app_state
        .domain_service
        .on_demand_cert_status(&hostname)
        .await
        .map_err(|e| {
            error!(
                "Failed to get on-demand cert status for {}: {}",
                hostname, e
            );
            e
        })?;

    let (domain_status, backoff_until) = match status.domain {
        Some(d) => (
            Some(d.status),
            d.on_demand_backoff_until.map(|dt| dt.timestamp_millis()),
        ),
        None => (None, None),
    };

    Ok((
        StatusCode::OK,
        Json(CertStatusResponse {
            hostname,
            status: domain_status,
            backoff_until,
            last_attempt: status.latest_attempt.map(OnDemandCertAttemptResponse::from),
        }),
    ))
}

pub fn configure_routes() -> Router<Arc<DomainAppState>> {
    Router::new()
        .route("/domains", post(create_domain))
        .route("/domains", get(list_domains))
        // On-demand cert observability (ADR-018 §5). NOTE: declared before the
        // `/domains/{domain}` catch-all so `on-demand-certs` is not captured as
        // a `{domain}` path parameter.
        .route("/domains/on-demand-certs", get(list_on_demand_certs))
        .route("/domains/{domain}", get(get_domain_by_id))
        .route("/domains/{domain}/status", get(check_domain_status))
        .route(
            "/domains/by-host/{hostname}/cert-status",
            get(get_on_demand_cert_status),
        )
        .route("/domains/by-host/{hostname}", get(get_domain_by_host))
        // Domain-based routes (using domain name)
        .route("/domains/{domain}", delete(delete_domain))
        .route("/domains/{domain}/provision", post(provision_domain))
        .route("/domains/{domain}/renew", post(renew_domain))
        .route(
            "/domains/{domain}/renewal-attempts",
            get(list_renewal_attempts),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use temps_dns::providers::{
        DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
        DnsRecordRequest, DnsRecordType, DnsZone,
    };
    use temps_dns::DnsError;

    fn test_auth(role: temps_auth::Role) -> temps_auth::AuthContext {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 42,
            name: "CLI Operator".to_string(),
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
        };
        temps_auth::AuthContext::new_session(user, role)
    }

    #[test]
    fn dns_cleanup_permission_denial_precedes_cleanup_execution() {
        let denied = ensure_dns_cleanup_permission(&test_auth(temps_auth::Role::Reader), true)
            .expect_err("reader must not mutate DNS during finalization");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        assert!(ensure_dns_cleanup_permission(&test_auth(temps_auth::Role::Reader), false).is_ok());
        assert!(ensure_dns_cleanup_permission(&test_auth(temps_auth::Role::Admin), true).is_ok());
    }

    #[test]
    fn active_domain_with_cleanup_receipt_retries_without_acme_reissuance() {
        assert!(should_retry_dns_cleanup_without_issuance("active", true));
        assert!(!should_retry_dns_cleanup_without_issuance("pending", true));
        assert!(!should_retry_dns_cleanup_without_issuance("active", false));
    }

    #[test]
    fn inactive_dns_provider_is_rejected_with_context() {
        let now = chrono::Utc::now();
        let provider = temps_entities::dns_providers::Model {
            id: 42,
            name: "disabled-cloudflare".to_string(),
            provider_type: "cloudflare".to_string(),
            credentials: "encrypted".to_string(),
            is_active: false,
            description: None,
            last_used_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        };

        let problem = ensure_dns_provider_active(&provider, "api.example.com", 17)
            .expect_err("inactive providers must be rejected before DNS setup");

        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            problem.body.get("title").and_then(|value| value.as_str()),
            Some("DNS Provider Is Inactive")
        );
        let detail = problem.body.get("detail").and_then(|value| value.as_str());
        assert!(detail.is_some_and(|detail| {
            detail.contains("42 (disabled-cloudflare)")
                && detail.contains("api.example.com")
                && detail.contains("ID 17")
        }));
    }

    /// In-memory DNS provider used to drive `setup_dns_txt_records` end-to-end without
    /// a live Cloudflare/Route53/etc. account. Mirrors the real providers' semantics:
    /// `list_records`/`remove_record` see every record in the zone, `create_record`
    /// always appends (multiple records may share a name, as ACME wildcard orders need).
    struct MockDnsProvider {
        records: Mutex<Vec<DnsRecord>>,
        next_id: AtomicU32,
        fail_create_after: Option<u32>,
        fail_delete_id: Option<String>,
        provider_type: DnsProviderType,
    }

    #[derive(Default)]
    struct RecordingCleanupReceiptStore {
        saved: Mutex<Vec<crate::tls::models::AcmeOrder>>,
        deleted: Mutex<Vec<String>>,
        fail_save: bool,
        fail_delete: bool,
    }

    #[async_trait]
    impl DnsCleanupReceiptStore for RecordingCleanupReceiptStore {
        async fn save_cleanup_order(
            &self,
            order: crate::tls::models::AcmeOrder,
        ) -> Result<(), RepositoryError> {
            if self.fail_save {
                return Err(RepositoryError::Database(
                    "injected receipt save failure".to_string(),
                ));
            }
            self.saved.lock().unwrap().push(order);
            Ok(())
        }

        async fn delete_cleanup_order(&self, order_url: &str) -> Result<(), RepositoryError> {
            if self.fail_delete {
                return Err(RepositoryError::Database(
                    "injected receipt delete failure".to_string(),
                ));
            }
            self.deleted.lock().unwrap().push(order_url.to_string());
            Ok(())
        }
    }

    impl MockDnsProvider {
        fn new() -> Self {
            Self {
                records: Mutex::new(Vec::new()),
                next_id: AtomicU32::new(1),
                fail_create_after: None,
                fail_delete_id: None,
                provider_type: DnsProviderType::Cloudflare,
            }
        }

        fn seed(records: Vec<DnsRecord>) -> Self {
            let provider = Self::new();
            *provider.records.lock().unwrap() = records;
            provider
        }

        fn failing_after(successful_creates: u32) -> Self {
            Self {
                records: Mutex::new(Vec::new()),
                next_id: AtomicU32::new(1),
                fail_create_after: Some(successful_creates),
                fail_delete_id: None,
                provider_type: DnsProviderType::Cloudflare,
            }
        }

        fn with_provider_type(mut self, provider_type: DnsProviderType) -> Self {
            self.provider_type = provider_type;
            self
        }

        fn failing_delete(mut self, record_id: &str) -> Self {
            self.fail_delete_id = Some(record_id.to_string());
            self
        }

        fn record_names(&self) -> Vec<(String, String)> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .map(|r| (r.name.clone(), r.content.to_value_string()))
                .collect()
        }
    }

    fn txt_record(id: &str, name: &str, content: &str) -> DnsRecord {
        DnsRecord {
            id: Some(id.to_string()),
            zone: "example.com".to_string(),
            name: name.to_string(),
            fqdn: format!("{}.example.com", name),
            content: DnsRecordContent::TXT {
                content: content.to_string(),
            },
            ttl: 120,
            proxied: false,
            metadata: HashMap::new(),
        }
    }

    #[async_trait]
    impl DnsProvider for MockDnsProvider {
        fn provider_type(&self) -> DnsProviderType {
            self.provider_type
        }

        fn capabilities(&self) -> DnsProviderCapabilities {
            DnsProviderCapabilities::default()
        }

        async fn test_connection(&self) -> Result<bool, DnsError> {
            Ok(true)
        }

        async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
            Ok(vec![])
        }

        async fn get_zone(&self, _domain: &str) -> Result<Option<DnsZone>, DnsError> {
            Ok(None)
        }

        async fn list_records(&self, _domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
            Ok(self.records.lock().unwrap().clone())
        }

        async fn get_record(
            &self,
            _domain: &str,
            name: &str,
            record_type: DnsRecordType,
        ) -> Result<Option<DnsRecord>, DnsError> {
            Ok(self
                .records
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.name == name && r.content.record_type() == record_type)
                .cloned())
        }

        async fn create_record(
            &self,
            domain: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst).to_string();
            if self
                .fail_create_after
                .is_some_and(|limit| id.parse::<u32>().unwrap_or(u32::MAX) > limit)
            {
                return Err(DnsError::ApiError("injected create failure".to_string()));
            }
            let record = DnsRecord {
                id: Some(id),
                zone: domain.to_string(),
                fqdn: format!("{}.{}", request.name, domain),
                name: request.name,
                content: request.content,
                ttl: request.ttl.unwrap_or(300),
                proxied: request.proxied,
                metadata: HashMap::new(),
            };
            self.records.lock().unwrap().push(record.clone());
            Ok(record)
        }

        async fn update_record(
            &self,
            _domain: &str,
            record_id: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            let mut records = self.records.lock().unwrap();
            let record = records
                .iter_mut()
                .find(|r| r.id.as_deref() == Some(record_id))
                .ok_or_else(|| DnsError::RecordNotFound(record_id.to_string()))?;
            record.content = request.content;
            record.name = request.name;
            Ok(record.clone())
        }

        async fn delete_record(&self, _domain: &str, record_id: &str) -> Result<(), DnsError> {
            if self.fail_delete_id.as_deref() == Some(record_id) {
                return Err(DnsError::ApiError("injected delete failure".to_string()));
            }
            let mut records = self.records.lock().unwrap();
            let before = records.len();
            records.retain(|r| r.id.as_deref() != Some(record_id));
            if records.len() == before {
                return Err(DnsError::RecordNotFound(record_id.to_string()));
            }
            Ok(())
        }
    }

    fn test_acme_order(authorizations: Option<serde_json::Value>) -> crate::tls::models::AcmeOrder {
        let now = chrono::Utc::now();
        crate::tls::models::AcmeOrder {
            id: 11,
            order_url: "https://acme.example.test/order/11".to_string(),
            domain_id: 17,
            email: "operator@example.test".to_string(),
            status: "pending".to_string(),
            identifiers: serde_json::json!([]),
            authorizations,
            finalize_url: None,
            certificate_url: None,
            error: None,
            error_type: None,
            token: None,
            key_authorization: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
        }
    }

    #[test]
    fn dns_cleanup_plan_round_trips_through_order_metadata() {
        let records = vec![
            DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "order-token".to_string(),
                record_id: "provider-record-7".to_string(),
            },
            DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "wildcard-order-token".to_string(),
                record_id: "provider-record-7".to_string(),
            },
            DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "order-token".to_string(),
                record_id: "provider-record-7".to_string(),
            },
        ];
        let plan = dns_cleanup_plan(42, "example.com.", records);
        let mut order = test_acme_order(Some(serde_json::json!({
            "dns_txt_records": [{"name": "_acme-challenge.example.com", "value": "order-token"}]
        })));

        store_dns_cleanup_plan(&mut order, &plan).expect("cleanup metadata should serialize");

        assert_eq!(load_dns_cleanup_plan(&order).unwrap(), Some(plan.clone()));
        assert_eq!(plan.zone, "example.com");
        assert_eq!(
            plan.records.len(),
            2,
            "exact duplicates must be removed without losing sibling RRset values"
        );
        assert_eq!(plan.records[0].record_id, "provider-record-7");
        assert_eq!(plan.records[1].value, "wildcard-order-token");
        assert!(dns_provision_requires_cleanup_aware_finalize(&order)
            .expect("cleanup-aware provision guard should decode metadata"));
        assert!(
            !dns_provision_requires_cleanup_aware_finalize(&test_acme_order(None))
                .expect("orders without cleanup metadata should use the legacy path")
        );
    }

    #[test]
    fn acme_order_responses_use_epoch_milliseconds() {
        let mut order = test_acme_order(None);
        order.expires_at = Some(order.created_at + chrono::Duration::hours(1));
        let expected_created_at = order.created_at.timestamp_millis();
        let expected_updated_at = order.updated_at.timestamp_millis();
        let expected_expires_at = order.expires_at.map(|value| value.timestamp_millis());

        let response = AcmeOrderResponse::from(order);

        assert_eq!(response.created_at, expected_created_at);
        assert_eq!(response.updated_at, expected_updated_at);
        assert_eq!(response.expires_at, expected_expires_at);
        assert!(response.created_at > 1_000_000_000_000);
    }

    #[tokio::test]
    async fn cleanup_removes_only_the_exact_acme_order_value() {
        let provider = MockDnsProvider::seed(vec![
            txt_record("1", "_acme-challenge", "completed-order-token"),
            txt_record("2", "_acme-challenge", "concurrent-order-token"),
            txt_record("3", "www", "unrelated"),
        ]);
        let plan = dns_cleanup_plan(
            42,
            "example.com",
            vec![DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "completed-order-token".to_string(),
                record_id: "1".to_string(),
            }],
        );

        let outcome = cleanup_dns_txt_records(&provider, &plan)
            .await
            .expect("record cleanup should succeed");

        assert_eq!(outcome.deleted, 1);
        assert!(outcome.errors.is_empty());
        assert!(outcome.remaining_records.is_empty());
        let remaining = provider.record_names();
        assert!(!remaining
            .iter()
            .any(|(_, value)| value == "completed-order-token"));
        assert!(remaining
            .iter()
            .any(|(_, value)| value == "concurrent-order-token"));
        assert!(remaining.iter().any(|(name, _)| name == "www"));
    }

    #[tokio::test]
    async fn cleanup_skips_providers_with_shared_txt_record_set_ids() {
        let provider = MockDnsProvider::seed(vec![
            txt_record(
                "_acme-challenge::TXT",
                "_acme-challenge",
                "completed-order-token",
            ),
            txt_record(
                "_acme-challenge::TXT",
                "_acme-challenge",
                "concurrent-order-token",
            ),
        ])
        .with_provider_type(DnsProviderType::Azure);
        let plan = dns_cleanup_plan(
            42,
            "example.com",
            vec![DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "completed-order-token".to_string(),
                record_id: "_acme-challenge::TXT".to_string(),
            }],
        );

        let error = cleanup_dns_txt_records(&provider, &plan)
            .await
            .expect_err("shared-record-set providers must not delete by synthetic ID");

        assert!(matches!(
            error,
            DnsCleanupError::SharedRecordSet {
                provider: DnsProviderType::Azure
            }
        ));
        assert_eq!(provider.record_names().len(), 2);
    }

    #[tokio::test]
    async fn cleanup_allows_local_test_only_pebble_provider() {
        let provider = MockDnsProvider::seed(vec![txt_record(
            "_acme-challenge.example.com.",
            "_acme-challenge",
            "completed-order-token",
        )])
        .with_provider_type(DnsProviderType::Pebble);
        let plan = dns_cleanup_plan(
            42,
            "example.com",
            vec![DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "completed-order-token".to_string(),
                record_id: "_acme-challenge.example.com.".to_string(),
            }],
        );

        let outcome = cleanup_dns_txt_records(&provider, &plan)
            .await
            .expect("the gated local Pebble provider should clear its test RRset");

        assert_eq!(outcome.deleted, 1);
        assert!(outcome.errors.is_empty());
        assert!(outcome.remaining_records.is_empty());
        assert!(provider.record_names().is_empty());
    }

    #[tokio::test]
    async fn cleanup_reports_only_failed_records_for_a_safe_retry() {
        let provider = MockDnsProvider::seed(vec![
            txt_record("1", "_acme-challenge", "first-token"),
            txt_record("2", "_acme-challenge", "second-token"),
        ])
        .failing_delete("2");
        let plan = dns_cleanup_plan(
            42,
            "example.com",
            vec![
                DnsCleanupRecord {
                    name: "_acme-challenge".to_string(),
                    value: "first-token".to_string(),
                    record_id: "1".to_string(),
                },
                DnsCleanupRecord {
                    name: "_acme-challenge".to_string(),
                    value: "second-token".to_string(),
                    record_id: "2".to_string(),
                },
            ],
        );

        let outcome = cleanup_dns_txt_records(&provider, &plan)
            .await
            .expect("safe providers return a cleanup outcome");

        assert_eq!(outcome.deleted, 1);
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.remaining_records.len(), 1);
        assert_eq!(outcome.remaining_records[0].record_id, "2");
        assert_eq!(
            provider.record_names(),
            vec![("_acme-challenge".to_string(), "second-token".to_string())]
        );
    }

    #[tokio::test]
    async fn cleanup_treats_an_already_absent_provider_record_as_complete() {
        let provider = MockDnsProvider::new();
        let plan = dns_cleanup_plan(
            42,
            "example.com",
            vec![DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "already-removed-token".to_string(),
                record_id: "gone".to_string(),
            }],
        );

        let outcome = cleanup_dns_txt_records(&provider, &plan)
            .await
            .expect("safe providers make missing-record cleanup idempotent");

        assert_eq!(outcome.deleted, 1);
        assert!(outcome.errors.is_empty());
        assert!(outcome.remaining_records.is_empty());
    }

    #[tokio::test]
    async fn cleanup_retains_pre_mutation_intent_without_a_provider_record_id() {
        let provider = MockDnsProvider::new();
        let plan = dns_cleanup_plan(
            42,
            "example.com",
            vec![DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "durable-intent-token".to_string(),
                record_id: String::new(),
            }],
        );

        let error = cleanup_dns_txt_records(&provider, &plan)
            .await
            .expect_err("missing provider IDs require explicit manual cleanup");

        assert!(matches!(
            error,
            DnsCleanupError::MissingRecordId { zone, name }
                if zone == "example.com" && name == "_acme-challenge"
        ));
        assert!(provider.record_names().is_empty());
    }

    #[tokio::test]
    async fn cleanup_workflow_deletes_exact_records_then_clears_the_receipt() {
        let provider =
            MockDnsProvider::seed(vec![txt_record("1", "_acme-challenge", "issued-token")]);
        let store = RecordingCleanupReceiptStore::default();
        let order = test_acme_order(None);
        let order_url = order.order_url.clone();
        let plan = dns_cleanup_plan(
            42,
            "example.com",
            vec![DnsCleanupRecord {
                name: "_acme-challenge".to_string(),
                value: "issued-token".to_string(),
                record_id: "1".to_string(),
            }],
        );

        let resolution = execute_dns_cleanup(&provider, &store, order, plan)
            .await
            .expect("cleanup workflow should persist its terminal transition");

        assert!(matches!(
            resolution,
            DnsCleanupResolution::Complete { deleted: 1 }
        ));
        assert!(provider.record_names().is_empty());
        assert!(store.saved.lock().unwrap().is_empty());
        assert_eq!(store.deleted.lock().unwrap().as_slice(), &[order_url]);
    }

    #[tokio::test]
    async fn cleanup_workflow_persists_only_failures_then_retries_to_completion() {
        let first_provider = MockDnsProvider::seed(vec![
            txt_record("1", "_acme-challenge", "first-token"),
            txt_record("2", "_acme-challenge", "second-token"),
        ])
        .failing_delete("2");
        let store = RecordingCleanupReceiptStore::default();
        let order = test_acme_order(None);
        let plan = dns_cleanup_plan(
            42,
            "example.com",
            vec![
                DnsCleanupRecord {
                    name: "_acme-challenge".to_string(),
                    value: "first-token".to_string(),
                    record_id: "1".to_string(),
                },
                DnsCleanupRecord {
                    name: "_acme-challenge".to_string(),
                    value: "second-token".to_string(),
                    record_id: "2".to_string(),
                },
            ],
        );

        let first_resolution = execute_dns_cleanup(&first_provider, &store, order, plan)
            .await
            .expect("partial cleanup should save retry progress");
        assert!(matches!(
            first_resolution,
            DnsCleanupResolution::Pending { ref errors } if errors.len() == 1
        ));

        let retry_order = store
            .saved
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("partial cleanup should retain the order");
        let retry_plan = load_dns_cleanup_plan(&retry_order)
            .expect("retry receipt should decode")
            .expect("retry receipt should exist");
        assert_eq!(retry_plan.records.len(), 1);
        assert_eq!(retry_plan.records[0].record_id, "2");

        let retry_provider =
            MockDnsProvider::seed(vec![txt_record("2", "_acme-challenge", "second-token")]);
        let retry_resolution =
            execute_dns_cleanup(&retry_provider, &store, retry_order, retry_plan)
                .await
                .expect("retry should clear the retained receipt");

        assert!(matches!(
            retry_resolution,
            DnsCleanupResolution::Complete { deleted: 1 }
        ));
        assert!(retry_provider.record_names().is_empty());
        assert_eq!(store.deleted.lock().unwrap().len(), 1);
    }

    // ==================== acme_txt_record_name ====================

    #[test]
    fn test_acme_txt_record_name_subdomain() {
        assert_eq!(
            acme_txt_record_name("example.com", "_acme-challenge.example.com"),
            "_acme-challenge"
        );
    }

    #[test]
    fn test_acme_txt_record_name_apex() {
        assert_eq!(acme_txt_record_name("example.com", "example.com"), "@");
    }

    // ==================== setup_dns_txt_records ====================

    #[tokio::test]
    async fn test_wildcard_batch_keeps_both_sibling_records() {
        // A wildcard order (*.example.com + example.com) publishes two TXT records
        // under the same `_acme-challenge` name, one per authorization. Cleanup must
        // not delete the first once the second is created in the same batch.
        let provider = MockDnsProvider::new();
        let dns_txt_records = vec![
            (
                "_acme-challenge.example.com".to_string(),
                "token-wildcard".to_string(),
            ),
            (
                "_acme-challenge.example.com".to_string(),
                "token-base".to_string(),
            ),
        ];

        let (results, records_created) =
            setup_dns_txt_records(&provider, "example.com", &dns_txt_records).await;

        assert_eq!(records_created, 2);
        assert!(results.iter().all(|r| r.success));

        let remaining = provider.record_names();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().any(|(_, v)| v == "token-wildcard"));
        assert!(remaining.iter().any(|(_, v)| v == "token-base"));
    }

    #[tokio::test]
    async fn setup_captures_exact_provider_record_ids_for_cleanup() {
        let provider = MockDnsProvider::new();
        let dns_txt_records = vec![
            (
                "_acme-challenge.example.com".to_string(),
                "token-wildcard".to_string(),
            ),
            (
                "_acme-challenge.example.com".to_string(),
                "token-base".to_string(),
            ),
        ];

        let outcome =
            setup_dns_txt_records_with_cleanup(&provider, "example.com", &dns_txt_records).await;

        assert_eq!(outcome.records_created, 2);
        assert_eq!(outcome.cleanup_records.len(), 2);
        assert_eq!(outcome.cleanup_records[0].record_id, "1");
        assert_eq!(outcome.cleanup_records[0].value, "token-wildcard");
        assert_eq!(outcome.cleanup_records[1].record_id, "2");
        assert_eq!(outcome.cleanup_records[1].value, "token-base");
    }

    #[tokio::test]
    async fn setup_workflow_persists_intent_before_replacing_it_with_exact_ids() {
        let provider = MockDnsProvider::new();
        let store = RecordingCleanupReceiptStore::default();
        let records = vec![
            (
                "_acme-challenge.example.com".to_string(),
                "token-wildcard".to_string(),
            ),
            (
                "_acme-challenge.example.com".to_string(),
                "token-base".to_string(),
            ),
        ];

        let outcome = execute_dns_setup(
            &provider,
            &store,
            test_acme_order(None),
            42,
            "example.com",
            &records,
        )
        .await
        .expect("setup should persist both lifecycle transitions");

        assert_eq!(outcome.records_created, 2);
        let saved = store.saved.lock().unwrap();
        assert_eq!(saved.len(), 2);
        let intent = load_dns_cleanup_plan(&saved[0])
            .expect("intent should decode")
            .expect("intent should exist");
        assert_eq!(intent.records.len(), 2);
        assert!(intent
            .records
            .iter()
            .all(|record| record.record_id.is_empty()));
        let exact = load_dns_cleanup_plan(&saved[1])
            .expect("exact receipt should decode")
            .expect("exact receipt should exist");
        assert_eq!(
            exact
                .records
                .iter()
                .map(|record| record.record_id.as_str())
                .collect::<Vec<_>>(),
            vec!["1", "2"]
        );
    }

    #[tokio::test]
    async fn setup_workflow_does_not_mutate_dns_when_intent_save_fails() {
        let provider = MockDnsProvider::new();
        let store = RecordingCleanupReceiptStore {
            fail_save: true,
            ..Default::default()
        };
        let records = vec![(
            "_acme-challenge.example.com".to_string(),
            "must-not-be-created".to_string(),
        )];

        let error = execute_dns_setup(
            &provider,
            &store,
            test_acme_order(None),
            42,
            "example.com",
            &records,
        )
        .await
        .expect_err("intent persistence failure must stop before DNS mutation");

        assert!(matches!(error, DnsSetupProgressError::IntentSave { .. }));
        assert!(!error.records_may_have_changed());
        assert!(provider.record_names().is_empty());
    }

    #[tokio::test]
    async fn test_renewal_batch_removes_stale_records_from_prior_order() {
        // Simulates a renewal: the zone already has TXT records from a previous
        // order (different values, same name) that must be gone after this batch,
        // replaced by exactly the new batch's records.
        let provider = MockDnsProvider::seed(vec![
            txt_record("1", "_acme-challenge", "stale-token-a"),
            txt_record("2", "_acme-challenge", "stale-token-b"),
            txt_record("3", "www", "unrelated"),
        ]);
        let dns_txt_records = vec![(
            "_acme-challenge.example.com".to_string(),
            "fresh-token".to_string(),
        )];

        let (results, records_created) =
            setup_dns_txt_records(&provider, "example.com", &dns_txt_records).await;

        assert_eq!(records_created, 1);
        assert!(results.iter().all(|r| r.success));

        let remaining = provider.record_names();
        assert_eq!(remaining.len(), 2);
        assert!(remaining
            .iter()
            .any(|(n, v)| n == "_acme-challenge" && v == "fresh-token"));
        assert!(remaining.iter().any(|(n, _)| n == "www"));
    }

    struct TestAutomationGate {
        decision: Result<temps_core::DnsAutomationDecision, String>,
    }

    struct PanicAutomationGate;

    #[async_trait]
    impl temps_core::DnsAutomationGate for PanicAutomationGate {
        async fn authorize(
            &self,
            _request: &temps_core::DnsAutomationRequest,
        ) -> Result<temps_core::DnsAutomationDecision, temps_core::DnsAutomationError> {
            panic!("invalid requests must be rejected before the authorization gate")
        }
    }

    #[async_trait]
    impl temps_core::DnsAutomationGate for TestAutomationGate {
        async fn authorize(
            &self,
            request: &temps_core::DnsAutomationRequest,
        ) -> Result<temps_core::DnsAutomationDecision, temps_core::DnsAutomationError> {
            self.decision.clone().map_err(|reason| {
                temps_core::DnsAutomationError::policy_evaluation_failed(request, reason)
            })
        }
    }

    fn automation_request(records: &[(String, String)]) -> temps_core::DnsAutomationRequest {
        temps_core::DnsAutomationRequest {
            purpose: temps_core::DnsAutomationPurpose::AcmeDns01,
            domain: "*.example.com".to_string(),
            zone: "example.com".to_string(),
            provider_id: 7,
            provider_name: "test".to_string(),
            mutations: records
                .iter()
                .map(|(name, value)| temps_core::DnsAutomationMutation {
                    record_type: "TXT".to_string(),
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn denied_automation_does_not_touch_provider() {
        let provider = MockDnsProvider::seed(vec![txt_record("1", "_acme-challenge", "stale")]);
        let records = vec![(
            "_acme-challenge.example.com".to_string(),
            "fresh".to_string(),
        )];
        let result = authorize_dns_automation_request(
            &TestAutomationGate {
                decision: Ok(temps_core::DnsAutomationDecision::Deny {
                    reason: "fresh".to_string(),
                }),
            },
            &automation_request(&records),
            7,
        )
        .await;

        assert!(matches!(
            result,
            DnsAutomationAuthorization::Denied(reason)
                if reason == "automation policy denied the request" && !reason.contains("fresh")
        ));
        assert_eq!(
            provider.record_names(),
            vec![("_acme-challenge".to_string(), "stale".to_string())]
        );
    }

    #[tokio::test]
    async fn authorization_error_does_not_touch_provider() {
        let provider = MockDnsProvider::seed(vec![txt_record("1", "_acme-challenge", "stale")]);
        let records = vec![(
            "_acme-challenge.example.com".to_string(),
            "fresh".to_string(),
        )];
        let result = authorize_dns_automation_request(
            &TestAutomationGate {
                decision: Err("fresh".to_string()),
            },
            &automation_request(&records),
            7,
        )
        .await;

        assert!(matches!(
            result,
            DnsAutomationAuthorization::AuthorizationError(reason)
                if reason == "automation policy evaluation failed" && !reason.contains("fresh")
        ));
        assert_eq!(provider.record_names().len(), 1);
    }

    #[tokio::test]
    async fn invalid_automation_requests_do_not_touch_gate_or_provider() {
        let invalid_cases = [
            automation_request(&[]),
            automation_request(&[("_acme-challenge.example.com".to_string(), " ".to_string())]),
            automation_request(&[(
                "_acme-challenge.attacker.example".to_string(),
                "token".to_string(),
            )]),
        ];

        for request in invalid_cases {
            let provider = MockDnsProvider::seed(vec![txt_record("1", "_acme-challenge", "stale")]);
            let result = authorize_dns_automation_request(&PanicAutomationGate, &request, 7).await;

            assert!(matches!(result, DnsAutomationAuthorization::Denied(_)));
            assert_eq!(
                provider.record_names(),
                vec![("_acme-challenge".to_string(), "stale".to_string())]
            );
        }
    }

    #[tokio::test]
    async fn provider_identity_mismatch_does_not_touch_gate_or_provider() {
        let provider = MockDnsProvider::new();
        let request = automation_request(&[(
            "_acme-challenge.example.com".to_string(),
            "token".to_string(),
        )]);

        let result = authorize_dns_automation_request(&PanicAutomationGate, &request, 99).await;

        assert!(matches!(result, DnsAutomationAuthorization::Denied(_)));
        assert!(provider.record_names().is_empty());
    }

    #[tokio::test]
    async fn allowed_automation_replaces_only_acme_txt_records() {
        let provider = MockDnsProvider::seed(vec![
            txt_record("1", "_acme-challenge", "stale"),
            txt_record("2", "www", "unrelated"),
        ]);
        let records = vec![(
            "_acme-challenge.example.com".to_string(),
            "fresh".to_string(),
        )];
        let authorization = authorize_dns_automation_request(
            &TestAutomationGate {
                decision: Ok(temps_core::DnsAutomationDecision::Allow),
            },
            &automation_request(&records),
            7,
        )
        .await;
        assert!(matches!(authorization, DnsAutomationAuthorization::Allowed));
        let (results, records_created) =
            setup_dns_txt_records(&provider, "example.com", &records).await;
        assert_eq!(records_created, 1);
        assert!(results.iter().all(|result| result.success));
        let remaining = provider.record_names();
        assert!(remaining
            .iter()
            .any(|(name, value)| { name == "_acme-challenge" && value == "fresh" }));
        assert!(remaining.iter().any(|(name, _)| name == "www"));
    }

    #[tokio::test]
    async fn authorized_setup_uses_the_request_authoritative_zone() {
        let provider = MockDnsProvider::new();
        let mut request = automation_request(&[(
            "_acme-challenge.api.dev.example.com".to_string(),
            "fresh".to_string(),
        )]);
        request.domain = "api.dev.example.com".to_string();
        request.zone = "dev.example.com".to_string();

        let authorization = authorize_dns_automation_request(
            &TestAutomationGate {
                decision: Ok(temps_core::DnsAutomationDecision::Allow),
            },
            &request,
            7,
        )
        .await;

        assert!(matches!(authorization, DnsAutomationAuthorization::Allowed));
        let records = request
            .mutations
            .iter()
            .map(|mutation| (mutation.name.clone(), mutation.value.clone()))
            .collect::<Vec<_>>();
        setup_dns_txt_records(&provider, &request.zone, &records).await;
        assert_eq!(
            provider.record_names(),
            vec![("_acme-challenge.api".to_string(), "fresh".to_string())]
        );
    }

    #[tokio::test]
    async fn partial_publish_failure_is_reported() {
        let provider = MockDnsProvider::failing_after(1);
        let records = vec![
            (
                "_acme-challenge.example.com".to_string(),
                "first".to_string(),
            ),
            (
                "_acme-challenge.example.com".to_string(),
                "second".to_string(),
            ),
        ];
        let request = automation_request(&records);
        let authorization = authorize_dns_automation_request(
            &TestAutomationGate {
                decision: Ok(temps_core::DnsAutomationDecision::Allow),
            },
            &request,
            7,
        )
        .await;
        assert!(matches!(authorization, DnsAutomationAuthorization::Allowed));
        let (results, records_created) =
            setup_dns_txt_records(&provider, &request.zone, &records).await;
        assert_eq!(records_created, 1);
        assert_eq!(results.len(), 2);
        assert!(!results[1].success);
    }
}
