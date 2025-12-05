//! HTTP handlers for DNS provider management
//!
//! This module contains the API endpoints for managing DNS providers,
//! managed domains, and DNS records.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_check, Permission, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use utoipa::{OpenApi, ToSchema};

use crate::errors::DnsError;
use crate::providers::{
    CloudflareCredentials, DnsProviderType, DnsRecord, DnsZone, NamecheapCredentials,
    ProviderCredentials,
};
use crate::services::{
    AddManagedDomainRequest, CreateProviderRequest, DnsProviderService, DnsRecordService,
    UpdateProviderRequest,
};

/// Application state for DNS handlers
pub struct DnsAppState {
    pub provider_service: Arc<DnsProviderService>,
    pub record_service: Arc<DnsRecordService>,
}

// ========================================
// Request/Response Types
// ========================================

/// Request to create a new DNS provider
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateDnsProviderRequest {
    /// User-friendly name
    #[schema(example = "My Cloudflare")]
    pub name: String,
    /// Provider type
    pub provider_type: DnsProviderType,
    /// Provider credentials
    pub credentials: DnsProviderCredentials,
    /// Optional description
    pub description: Option<String>,
}

/// Request to update a DNS provider
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateDnsProviderRequest {
    /// New name
    pub name: Option<String>,
    /// New credentials
    pub credentials: Option<DnsProviderCredentials>,
    /// New description
    pub description: Option<String>,
    /// Active status
    pub is_active: Option<bool>,
}

/// DNS provider credentials (API-facing)
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DnsProviderCredentials {
    Cloudflare {
        #[schema(example = "your-api-token")]
        api_token: String,
        account_id: Option<String>,
    },
    Namecheap {
        #[schema(example = "your-username")]
        api_user: String,
        #[schema(example = "your-api-key")]
        api_key: String,
        client_ip: Option<String>,
        #[serde(default)]
        sandbox: bool,
    },
}

impl From<DnsProviderCredentials> for ProviderCredentials {
    fn from(creds: DnsProviderCredentials) -> Self {
        match creds {
            DnsProviderCredentials::Cloudflare {
                api_token,
                account_id,
            } => ProviderCredentials::Cloudflare(CloudflareCredentials {
                api_token,
                account_id,
            }),
            DnsProviderCredentials::Namecheap {
                api_user,
                api_key,
                client_ip,
                sandbox,
            } => ProviderCredentials::Namecheap(NamecheapCredentials {
                api_user,
                api_key,
                client_ip,
                sandbox,
            }),
        }
    }
}

/// DNS provider response
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DnsProviderResponse {
    pub id: i32,
    pub name: String,
    pub provider_type: String,
    /// Masked credentials for display
    pub credentials: serde_json::Value,
    pub is_active: bool,
    pub description: Option<String>,
    pub last_used_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to add a managed domain
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AddManagedDomainApiRequest {
    #[schema(example = "example.com")]
    pub domain: String,
    #[serde(default = "default_true")]
    pub auto_manage: bool,
}

fn default_true() -> bool {
    true
}

/// Managed domain response
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagedDomainResponse {
    pub id: i32,
    pub provider_id: i32,
    pub domain: String,
    pub zone_id: Option<String>,
    pub auto_manage: bool,
    pub verified: bool,
    pub verified_at: Option<String>,
    pub verification_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Connection test result
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectionTestResult {
    pub success: bool,
    pub message: String,
}

/// Zone list response
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ZoneListResponse {
    pub zones: Vec<DnsZone>,
}

/// Record list response
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecordListResponse {
    pub records: Vec<DnsRecord>,
}

// ========================================
// Error Handling
// ========================================

impl From<DnsError> for Problem {
    fn from(error: DnsError) -> Self {
        match error {
            DnsError::ProviderNotFound(id) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Provider Not Found")
                .with_detail(format!("DNS provider with ID {} not found", id)),
            DnsError::DomainNotFound(domain) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Domain Not Found")
                .with_detail(format!("Domain {} not found", domain)),
            DnsError::ZoneNotFound(zone) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Zone Not Found")
                .with_detail(format!("DNS zone {} not found", zone)),
            DnsError::RecordNotFound(record) => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Record Not Found")
                .with_detail(format!("DNS record {} not found", record)),
            DnsError::InvalidProviderType(t) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Provider Type")
                .with_detail(format!("Unknown provider type: {}", t)),
            DnsError::InvalidCredentials(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Credentials")
                .with_detail(msg),
            DnsError::Validation(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(msg),
            DnsError::PermissionDenied(msg) => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Permission Denied")
                .with_detail(msg),
            DnsError::RateLimited(msg) => problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                .with_title("Rate Limited")
                .with_detail(msg),
            DnsError::NotSupported(msg) => problemdetails::new(StatusCode::NOT_IMPLEMENTED)
                .with_title("Not Supported")
                .with_detail(msg),
            DnsError::ApiError(msg) => problemdetails::new(StatusCode::BAD_GATEWAY)
                .with_title("API Error")
                .with_detail(msg),
            _ => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Error")
                .with_detail(error.to_string()),
        }
    }
}

// ========================================
// Handlers
// ========================================

/// List all DNS providers
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers",
    responses(
        (status = 200, description = "List of DNS providers", body = Vec<DnsProviderResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsRead);

    let providers = state.provider_service.list().await?;

    let responses: Vec<DnsProviderResponse> = providers
        .into_iter()
        .map(|p| {
            let masked_creds = state
                .provider_service
                .get_masked_credentials(&p)
                .unwrap_or_else(|_| serde_json::json!({}));

            DnsProviderResponse {
                id: p.id,
                name: p.name,
                provider_type: p.provider_type,
                credentials: masked_creds,
                is_active: p.is_active,
                description: p.description,
                last_used_at: p.last_used_at.map(|t| t.to_rfc3339()),
                last_error: p.last_error,
                created_at: p.created_at.to_rfc3339(),
                updated_at: p.updated_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(responses))
}

/// Create a new DNS provider
///
/// The provider's credentials will be tested before creation.
/// If the connection test fails, the provider will not be created.
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers",
    request_body = CreateDnsProviderRequest,
    responses(
        (status = 201, description = "DNS provider created", body = DnsProviderResponse),
        (status = 400, description = "Invalid request or connection test failed"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn create_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Json(request): Json<CreateDnsProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    let credentials: ProviderCredentials = request.credentials.into();

    // Test the credentials before creating the provider
    state
        .provider_service
        .test_credentials(&request.provider_type, &credentials)
        .await?;

    // Credentials are valid, create the provider
    let provider = state
        .provider_service
        .create(CreateProviderRequest {
            name: request.name,
            provider_type: request.provider_type,
            credentials,
            description: request.description,
        })
        .await?;

    let masked_creds = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));

    let response = DnsProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: provider.provider_type,
        credentials: masked_creds,
        is_active: provider.is_active,
        description: provider.description,
        last_used_at: provider.last_used_at.map(|t| t.to_rfc3339()),
        last_error: provider.last_error,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// Get a DNS provider by ID
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers/{id}",
    responses(
        (status = 200, description = "DNS provider details", body = DnsProviderResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsRead);

    let provider = state.provider_service.get(id).await?;

    let masked_creds = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));

    let response = DnsProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: provider.provider_type,
        credentials: masked_creds,
        is_active: provider.is_active,
        description: provider.description,
        last_used_at: provider.last_used_at.map(|t| t.to_rfc3339()),
        last_error: provider.last_error,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    Ok(Json(response))
}

/// Update a DNS provider
#[utoipa::path(
    tag = "DNS Providers",
    put,
    path = "/dns-providers/{id}",
    request_body = UpdateDnsProviderRequest,
    responses(
        (status = 200, description = "DNS provider updated", body = DnsProviderResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn update_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateDnsProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    let provider = state
        .provider_service
        .update(
            id,
            UpdateProviderRequest {
                name: request.name,
                credentials: request.credentials.map(|c| c.into()),
                description: request.description,
                is_active: request.is_active,
            },
        )
        .await?;

    let masked_creds = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));

    let response = DnsProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: provider.provider_type,
        credentials: masked_creds,
        is_active: provider.is_active,
        description: provider.description,
        last_used_at: provider.last_used_at.map(|t| t.to_rfc3339()),
        last_error: provider.last_error,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    Ok(Json(response))
}

/// Delete a DNS provider
#[utoipa::path(
    tag = "DNS Providers",
    delete,
    path = "/dns-providers/{id}",
    responses(
        (status = 204, description = "DNS provider deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn delete_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    state.provider_service.delete(id).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Test provider connection
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers/{id}/test",
    responses(
        (status = 200, description = "Connection test result", body = ConnectionTestResult),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn test_provider_connection(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    let success = state.provider_service.test_connection(id).await?;

    let response = ConnectionTestResult {
        success,
        message: if success {
            "Connection successful".to_string()
        } else {
            "Connection failed".to_string()
        },
    };

    Ok(Json(response))
}

/// List zones available in a provider
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers/{id}/zones",
    responses(
        (status = 200, description = "List of zones", body = ZoneListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_provider_zones(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsRead);

    let provider = state.provider_service.get(id).await?;
    let instance = state.provider_service.create_provider_instance(&provider)?;

    let zones = instance.list_zones().await?;

    Ok(Json(ZoneListResponse { zones }))
}

/// Add a managed domain to a provider
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers/{id}/domains",
    request_body = AddManagedDomainApiRequest,
    responses(
        (status = 201, description = "Managed domain added", body = ManagedDomainResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn add_managed_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
    Json(request): Json<AddManagedDomainApiRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    let managed = state
        .provider_service
        .add_managed_domain(
            id,
            AddManagedDomainRequest {
                domain: request.domain,
                auto_manage: request.auto_manage,
            },
        )
        .await?;

    let response = ManagedDomainResponse {
        id: managed.id,
        provider_id: managed.provider_id,
        domain: managed.domain,
        zone_id: managed.zone_id,
        auto_manage: managed.auto_manage,
        verified: managed.verified,
        verified_at: managed.verified_at.map(|t| t.to_rfc3339()),
        verification_error: managed.verification_error,
        created_at: managed.created_at.to_rfc3339(),
        updated_at: managed.updated_at.to_rfc3339(),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// List managed domains for a provider
#[utoipa::path(
    tag = "DNS Providers",
    get,
    path = "/dns-providers/{id}/domains",
    responses(
        (status = 200, description = "List of managed domains", body = Vec<ManagedDomainResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_managed_domains(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsRead);

    let domains = state.provider_service.list_managed_domains(id).await?;

    let responses: Vec<ManagedDomainResponse> = domains
        .into_iter()
        .map(|d| ManagedDomainResponse {
            id: d.id,
            provider_id: d.provider_id,
            domain: d.domain,
            zone_id: d.zone_id,
            auto_manage: d.auto_manage,
            verified: d.verified,
            verified_at: d.verified_at.map(|t| t.to_rfc3339()),
            verification_error: d.verification_error,
            created_at: d.created_at.to_rfc3339(),
            updated_at: d.updated_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(responses))
}

/// Remove a managed domain
#[utoipa::path(
    tag = "DNS Providers",
    delete,
    path = "/dns-providers/{provider_id}/domains/{domain}",
    responses(
        (status = 204, description = "Managed domain removed"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn remove_managed_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path((provider_id, domain)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    state
        .provider_service
        .remove_managed_domain(provider_id, &domain)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Verify a managed domain
#[utoipa::path(
    tag = "DNS Providers",
    post,
    path = "/dns-providers/{provider_id}/domains/{domain}/verify",
    responses(
        (status = 200, description = "Domain verification result", body = ManagedDomainResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn verify_managed_domain(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path((provider_id, domain)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    let _verified = state
        .provider_service
        .verify_managed_domain(provider_id, &domain)
        .await?;

    // Fetch the updated domain
    let domains = state
        .provider_service
        .list_managed_domains(provider_id)
        .await?;
    let managed = domains
        .into_iter()
        .find(|d| d.domain == domain)
        .ok_or_else(|| DnsError::DomainNotFound(domain))?;

    let response = ManagedDomainResponse {
        id: managed.id,
        provider_id: managed.provider_id,
        domain: managed.domain,
        zone_id: managed.zone_id,
        auto_manage: managed.auto_manage,
        verified: managed.verified,
        verified_at: managed.verified_at.map(|t| t.to_rfc3339()),
        verification_error: managed.verification_error,
        created_at: managed.created_at.to_rfc3339(),
        updated_at: managed.updated_at.to_rfc3339(),
    };

    Ok(Json(response))
}

// ========================================
// Router Configuration
// ========================================

/// Configure DNS routes
pub fn configure_routes() -> Router<Arc<DnsAppState>> {
    Router::new()
        // Provider management
        .route("/dns-providers", get(list_providers).post(create_provider))
        .route(
            "/dns-providers/{id}",
            get(get_provider)
                .put(update_provider)
                .delete(delete_provider),
        )
        .route("/dns-providers/{id}/test", post(test_provider_connection))
        .route("/dns-providers/{id}/zones", get(list_provider_zones))
        // Managed domains
        .route(
            "/dns-providers/{id}/domains",
            get(list_managed_domains).post(add_managed_domain),
        )
        .route(
            "/dns-providers/{provider_id}/domains/{domain}",
            delete(remove_managed_domain),
        )
        .route(
            "/dns-providers/{provider_id}/domains/{domain}/verify",
            post(verify_managed_domain),
        )
}

// ========================================
// OpenAPI Documentation
// ========================================

#[derive(OpenApi)]
#[openapi(
    paths(
        list_providers,
        create_provider,
        get_provider,
        update_provider,
        delete_provider,
        test_provider_connection,
        list_provider_zones,
        add_managed_domain,
        list_managed_domains,
        remove_managed_domain,
        verify_managed_domain,
    ),
    components(
        schemas(
            CreateDnsProviderRequest,
            UpdateDnsProviderRequest,
            DnsProviderCredentials,
            DnsProviderResponse,
            AddManagedDomainApiRequest,
            ManagedDomainResponse,
            ConnectionTestResult,
            ZoneListResponse,
            RecordListResponse,
            DnsProviderType,
            DnsZone,
            DnsRecord,
        )
    ),
    tags(
        (name = "DNS Providers", description = "DNS provider management endpoints")
    )
)]
pub struct DnsApiDoc;
