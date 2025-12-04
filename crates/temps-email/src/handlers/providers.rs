//! Email provider handlers

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::{bad_request, internal_server_error, not_found},
    problemdetails::Problem,
    AuditContext, RequestMetadata,
};
use tracing::error;

use super::audit::{
    EmailProviderCreatedAudit, EmailProviderDeletedAudit, EmailProviderTestedAudit,
};
use super::types::{
    AppState, CreateEmailProviderRequest, EmailProviderResponse, EmailProviderTypeRoute,
    TestEmailRequest, TestEmailResponse,
};
use crate::providers::{EmailProviderType, ScalewayCredentials, SesCredentials};
use crate::services::{CreateProviderRequest, ProviderCredentials};

/// Configure provider routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/email-providers",
            post(create_provider).get(list_providers),
        )
        .route(
            "/email-providers/{id}",
            get(get_provider).delete(delete_provider),
        )
        .route("/email-providers/{id}/test", post(test_provider))
}

/// Create a new email provider
#[utoipa::path(
    tag = "Email Providers",
    post,
    path = "/email-providers",
    request_body = CreateEmailProviderRequest,
    responses(
        (status = 201, description = "Provider created successfully", body = EmailProviderResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateEmailProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailProvidersCreate);

    // Validate and extract credentials
    let credentials = match request.provider_type {
        EmailProviderTypeRoute::Ses => {
            let ses_creds = request.ses_credentials.ok_or_else(|| {
                bad_request()
                    .detail("ses_credentials required for SES provider")
                    .build()
            })?;
            ProviderCredentials::Ses(SesCredentials {
                access_key_id: ses_creds.access_key_id,
                secret_access_key: ses_creds.secret_access_key,
                endpoint_url: None, // Custom endpoints not supported via API
            })
        }
        EmailProviderTypeRoute::Scaleway => {
            let scw_creds = request.scaleway_credentials.ok_or_else(|| {
                bad_request()
                    .detail("scaleway_credentials required for Scaleway provider")
                    .build()
            })?;
            ProviderCredentials::Scaleway(ScalewayCredentials {
                api_key: scw_creds.api_key,
                project_id: scw_creds.project_id,
            })
        }
    };

    let create_request = CreateProviderRequest {
        name: request.name.clone(),
        provider_type: EmailProviderType::from(request.provider_type),
        region: request.region.clone(),
        credentials,
    };

    let provider = state
        .provider_service
        .create(create_request)
        .await
        .map_err(|e| {
            error!("Failed to create email provider: {}", e);
            internal_server_error()
                .detail(format!("Failed to create provider: {}", e))
                .build()
        })?;

    // Get masked credentials for response
    let masked_credentials = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));

    // Create audit log
    let audit = EmailProviderCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        provider_id: provider.id,
        name: provider.name.clone(),
        provider_type: provider.provider_type.clone(),
        region: provider.region.clone(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = EmailProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: EmailProviderType::from_str(&provider.provider_type)
            .map(EmailProviderTypeRoute::from)
            .unwrap_or(EmailProviderTypeRoute::Ses),
        region: provider.region,
        is_active: provider.is_active,
        credentials: masked_credentials,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// List all email providers
#[utoipa::path(
    tag = "Email Providers",
    get,
    path = "/email-providers",
    responses(
        (status = 200, description = "List of email providers", body = Vec<EmailProviderResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_providers(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailProvidersRead);

    let providers = state.provider_service.list().await.map_err(|e| {
        error!("Failed to list email providers: {}", e);
        internal_server_error()
            .detail("Failed to list providers")
            .build()
    })?;

    let responses: Vec<EmailProviderResponse> = providers
        .into_iter()
        .map(|p| {
            let masked_credentials = state
                .provider_service
                .get_masked_credentials(&p)
                .unwrap_or_else(|_| serde_json::json!({}));

            EmailProviderResponse {
                id: p.id,
                name: p.name,
                provider_type: EmailProviderType::from_str(&p.provider_type)
                    .map(EmailProviderTypeRoute::from)
                    .unwrap_or(EmailProviderTypeRoute::Ses),
                region: p.region,
                is_active: p.is_active,
                credentials: masked_credentials,
                created_at: p.created_at.to_rfc3339(),
                updated_at: p.updated_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(responses))
}

/// Get an email provider by ID
#[utoipa::path(
    tag = "Email Providers",
    get,
    path = "/email-providers/{id}",
    responses(
        (status = 200, description = "Email provider details", body = EmailProviderResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailProvidersRead);

    let provider = state.provider_service.get(id).await.map_err(|e| {
        error!("Failed to get email provider: {}", e);
        not_found().detail("Provider not found").build()
    })?;

    let masked_credentials = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));

    let response = EmailProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: EmailProviderType::from_str(&provider.provider_type)
            .map(EmailProviderTypeRoute::from)
            .unwrap_or(EmailProviderTypeRoute::Ses),
        region: provider.region,
        is_active: provider.is_active,
        credentials: masked_credentials,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    Ok(Json(response))
}

/// Delete an email provider
#[utoipa::path(
    tag = "Email Providers",
    delete,
    path = "/email-providers/{id}",
    responses(
        (status = 204, description = "Provider deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailProvidersDelete);

    // Get provider details before deletion for audit log
    let provider = state.provider_service.get(id).await.map_err(|e| {
        error!("Failed to get email provider: {}", e);
        not_found().detail("Provider not found").build()
    })?;

    state.provider_service.delete(id).await.map_err(|e| {
        error!("Failed to delete email provider: {}", e);
        internal_server_error()
            .detail("Failed to delete provider")
            .build()
    })?;

    // Create audit log
    let audit = EmailProviderDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        provider_id: provider.id,
        name: provider.name,
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Test an email provider by sending a test email to the logged-in user
#[utoipa::path(
    tag = "Email Providers",
    post,
    path = "/email-providers/{id}/test",
    request_body = TestEmailRequest,
    responses(
        (status = 200, description = "Test email result", body = TestEmailResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn test_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<TestEmailRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailProvidersWrite);

    // Validate from address
    if request.from.is_empty() {
        return Err(bad_request().detail("From address is required").build());
    }

    // Get the user's email address from the auth context
    let recipient_email = auth.user.email.clone();

    // Get provider details for audit log
    let provider = state.provider_service.get(id).await.map_err(|e| {
        error!("Failed to get email provider: {}", e);
        not_found().detail("Provider not found").build()
    })?;

    // Send test email with from address from request
    let result = state
        .provider_service
        .send_test_email(
            id,
            &recipient_email,
            &request.from,
            request.from_name.as_deref(),
        )
        .await
        .map_err(|e| {
            error!("Failed to send test email: {}", e);
            internal_server_error()
                .detail(format!("Failed to send test email: {}", e))
                .build()
        })?;

    // Create audit log
    let audit = EmailProviderTestedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        provider_id: provider.id,
        name: provider.name,
        recipient_email: recipient_email.clone(),
        success: result.success,
        error: result.error.clone(),
    };

    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(TestEmailResponse {
        success: result.success,
        sent_to: result.recipient_email,
        provider_message_id: result.provider_message_id,
        error: result.error,
    }))
}
