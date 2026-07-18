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
    error_builder::{bad_request, forbidden, internal_server_error, not_found},
    problemdetails::Problem,
    AuditContext, RequestMetadata,
};
use tracing::error;

use super::audit::{
    EmailProviderCreatedAudit, EmailProviderDeletedAudit, EmailProviderTestedAudit,
    EmailProviderTrackingSetupAudit, EmailProviderUpdatedAudit,
};
use super::types::{
    AppState, CreateEmailProviderRequest, EmailProviderResponse, EmailProviderTypeRoute,
    EmailTrackingSetupResponse, EmailTrackingStatusResponse, TestEmailRequest, TestEmailResponse,
    UpdateEmailProviderRequest,
};
use crate::providers::{EmailProviderType, ScalewayCredentials, SesCredentials, SmtpCredentials};
use crate::services::{CreateProviderRequest, ProviderCredentials, UpdateProviderRequest};

/// Configure provider routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/email-providers",
            post(create_email_provider).get(list_email_providers),
        )
        .route(
            "/email-providers/{id}",
            get(get_email_provider)
                .patch(update_email_provider)
                .delete(delete_email_provider),
        )
        .route("/email-providers/{id}/test", post(test_provider))
        .route(
            "/email-providers/{id}/tracking/status",
            get(get_email_tracking_status),
        )
        .route(
            "/email-providers/{id}/tracking/setup",
            post(setup_email_tracking),
        )
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
pub async fn create_email_provider(
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
        EmailProviderTypeRoute::Smtp => {
            let smtp_creds = request.smtp_credentials.ok_or_else(|| {
                bad_request()
                    .detail("smtp_credentials required for SMTP provider")
                    .build()
            })?;
            if smtp_creds.host.trim().is_empty() {
                return Err(bad_request()
                    .detail("smtp_credentials.host is required")
                    .build());
            }
            if smtp_creds.port == 0 {
                return Err(bad_request()
                    .detail("smtp_credentials.port must be greater than 0")
                    .build());
            }
            ProviderCredentials::Smtp(SmtpCredentials {
                host: smtp_creds.host,
                port: smtp_creds.port,
                username: smtp_creds.username.filter(|s| !s.is_empty()),
                password: smtp_creds.password.filter(|s| !s.is_empty()),
                encryption: smtp_creds.encryption.into(),
                accept_invalid_certs: smtp_creds.accept_invalid_certs,
            })
        }
    };

    let create_request = CreateProviderRequest {
        name: request.name.clone(),
        provider_type: EmailProviderType::from(request.provider_type),
        region: request.region.clone(),
        credentials,
    };
    let sns_topic_arn = request.sns_topic_arn.clone();

    let provider = state
        .provider_service
        .create_with_sns_topic(create_request, sns_topic_arn)
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

    // Fire-and-forget anonymous telemetry — no identifying properties
    state.telemetry.report(
        temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::EmailProviderConfigured,
        )
        .with("provider", provider.provider_type.clone()),
    );

    let response = EmailProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: EmailProviderType::from_str(&provider.provider_type)
            .map(EmailProviderTypeRoute::from)
            .unwrap_or(EmailProviderTypeRoute::Ses),
        region: provider.region,
        sns_topic_arn: provider.sns_topic_arn,
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
pub async fn list_email_providers(
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
                sns_topic_arn: p.sns_topic_arn,
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
pub async fn get_email_provider(
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
        sns_topic_arn: provider.sns_topic_arn,
        is_active: provider.is_active,
        credentials: masked_credentials,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    Ok(Json(response))
}

/// Update an email provider
///
/// Partial update — any field left out keeps its current value. Most importantly,
/// omitting the credential block (`ses_credentials`/`scaleway_credentials`/`smtp_credentials`)
/// preserves the stored secret, so operators can rename a provider without re-typing
/// passwords. `provider_type` is immutable; to switch providers, delete and recreate.
#[utoipa::path(
    tag = "Email Providers",
    patch,
    path = "/email-providers/{id}",
    request_body = UpdateEmailProviderRequest,
    responses(
        (status = 200, description = "Provider updated", body = EmailProviderResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
        (status = 409, description = "Provider type mismatch"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_email_provider(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<UpdateEmailProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailProvidersWrite);

    // Look up the existing provider so we know its type, and so we can reject
    // mismatched credential payloads with a clear message instead of a 404.
    let existing = state.provider_service.get(id).await.map_err(|e| {
        error!("Failed to look up email provider {} for update: {}", id, e);
        not_found().detail("Provider not found").build()
    })?;
    let existing_type = EmailProviderType::from_str(&existing.provider_type).map_err(|e| {
        error!(
            "Stored provider {} has invalid provider_type '{}': {}",
            id, existing.provider_type, e
        );
        internal_server_error()
            .detail("Stored provider has an unrecognised provider_type")
            .build()
    })?;

    // Only one credential block is allowed at a time, and it must match the
    // existing provider's type. Reject extras up-front so the service layer's
    // mismatch error doesn't surprise the caller.
    let supplied_blocks = [
        request.ses_credentials.is_some(),
        request.scaleway_credentials.is_some(),
        request.smtp_credentials.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if supplied_blocks > 1 {
        return Err(bad_request()
            .detail("Only one credential block (ses/scaleway/smtp) may be supplied per update")
            .build());
    }

    let credentials: Option<ProviderCredentials> = match (
        existing_type,
        request.ses_credentials,
        request.scaleway_credentials,
        request.smtp_credentials,
    ) {
        (_, None, None, None) => None,
        (EmailProviderType::Ses, Some(c), None, None) => {
            Some(ProviderCredentials::Ses(SesCredentials {
                access_key_id: c.access_key_id,
                secret_access_key: c.secret_access_key,
                endpoint_url: None,
            }))
        }
        (EmailProviderType::Scaleway, None, Some(c), None) => {
            Some(ProviderCredentials::Scaleway(ScalewayCredentials {
                api_key: c.api_key,
                project_id: c.project_id,
            }))
        }
        (EmailProviderType::Smtp, None, None, Some(c)) => {
            if c.host.trim().is_empty() {
                return Err(bad_request()
                    .detail("smtp_credentials.host is required")
                    .build());
            }
            if c.port == 0 {
                return Err(bad_request()
                    .detail("smtp_credentials.port must be greater than 0")
                    .build());
            }
            Some(ProviderCredentials::Smtp(SmtpCredentials {
                host: c.host,
                port: c.port,
                username: c.username.filter(|s| !s.is_empty()),
                password: c.password.filter(|s| !s.is_empty()),
                encryption: c.encryption.into(),
                accept_invalid_certs: c.accept_invalid_certs,
            }))
        }
        (existing, _, _, _) => {
            return Err(bad_request()
                .detail(format!(
                    "Credential block does not match existing provider type ({})",
                    existing
                ))
                .build());
        }
    };

    let sns_topic_arn = request.sns_topic_arn;
    let update_request = UpdateProviderRequest {
        name: request.name,
        region: request.region,
        is_active: request.is_active,
        credentials,
    };

    let outcome = state
        .provider_service
        .update_with_sns_topic(id, update_request, sns_topic_arn)
        .await
        .map_err(|e: crate::errors::EmailError| -> Problem {
            error!("Failed to update email provider {}: {}", id, e);
            // Service-level validation errors (empty name, type mismatch) become 400.
            // Everything else goes through the general From<EmailError> mapping.
            e.into()
        })?;

    let provider = outcome.provider;
    let masked_credentials = state
        .provider_service
        .get_masked_credentials(&provider)
        .unwrap_or_else(|_| serde_json::json!({}));

    // Only emit an audit log if something actually changed.
    if !outcome.changed_fields.is_empty() {
        let audit = EmailProviderUpdatedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            provider_id: provider.id,
            name: provider.name.clone(),
            provider_type: provider.provider_type.clone(),
            changed_fields: outcome.changed_fields,
        };
        if let Err(e) = state.audit_service.create_audit_log(&audit).await {
            error!("Failed to create audit log: {}", e);
        }
    }

    let response = EmailProviderResponse {
        id: provider.id,
        name: provider.name,
        provider_type: EmailProviderType::from_str(&provider.provider_type)
            .map(EmailProviderTypeRoute::from)
            .unwrap_or(EmailProviderTypeRoute::Ses),
        region: provider.region,
        sns_topic_arn: provider.sns_topic_arn,
        is_active: provider.is_active,
        credentials: masked_credentials,
        created_at: provider.created_at.to_rfc3339(),
        updated_at: provider.updated_at.to_rfc3339(),
    };

    Ok((StatusCode::OK, Json(response)))
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
pub async fn delete_email_provider(
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
    // Deployment tokens are not allowed as we need a real email to send the test to
    let user = auth
        .require_user()
        .map_err(|msg| forbidden().detail(msg).build())?;
    let recipient_email = user.email.clone();

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

/// The public SES tracking webhook URL for the current external URL.
async fn tracking_webhook_url(state: &AppState) -> Result<String, Problem> {
    let external_url = state
        .config_service
        .get_external_url_or_default()
        .await
        .map_err(|e| {
            error!("Failed to resolve external URL: {}", e);
            internal_server_error()
                .detail("Failed to resolve the instance external URL")
                .build()
        })?;
    Ok(format!(
        "{}/api/t/webhook/ses",
        external_url.trim_end_matches('/')
    ))
}

/// Live status of SES event tracking for a provider
#[utoipa::path(
    tag = "Email Providers",
    get,
    path = "/email-providers/{id}/tracking/status",
    params(("id" = i32, Path, description = "Provider ID")),
    responses(
        (status = 200, description = "Event tracking status", body = EmailTrackingStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_email_tracking_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailProvidersRead);

    let provider = state.provider_service.get(id).await.map_err(|e| match e {
        crate::EmailError::ProviderNotFound(_) => not_found().detail("Provider not found").build(),
        other => {
            error!("Failed to load email provider {id}: {other}");
            internal_server_error()
                .detail("Failed to load provider")
                .build()
        }
    })?;

    let webhook_url = tracking_webhook_url(&state).await?;
    let supports_event_tracking = EmailProviderType::from_str(&provider.provider_type)
        .map(|t| t == EmailProviderType::Ses)
        .unwrap_or(false);

    let last_event_at = if supports_event_tracking {
        state
            .tracking_setup_service
            .last_provider_event_at(id)
            .await
            .map_err(|e| {
                error!("Failed to query last provider event: {}", e);
                internal_server_error()
                    .detail("Failed to query event history")
                    .build()
            })?
    } else {
        None
    };

    Ok(Json(EmailTrackingStatusResponse {
        webhook_url,
        supports_event_tracking,
        sns_topic_arn: provider.sns_topic_arn,
        subscription_confirmed_at: provider
            .sns_subscription_confirmed_at
            .map(|dt| dt.to_rfc3339()),
        last_event_at: last_event_at.map(|dt| dt.to_rfc3339()),
    }))
}

/// One-click AWS-side setup of SES event tracking (SNS topic + webhook
/// subscription + SESv2 event destination), using the provider's stored
/// credentials.
#[utoipa::path(
    tag = "Email Providers",
    post,
    path = "/email-providers/{id}/tracking/setup",
    params(("id" = i32, Path, description = "Provider ID")),
    responses(
        (status = 200, description = "Setup completed", body = EmailTrackingSetupResponse),
        (status = 400, description = "Provider does not support event tracking"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Provider not found"),
        (status = 502, description = "An AWS call failed — the response detail names the failed step")
    ),
    security(("bearer_auth" = []))
)]
pub async fn setup_email_tracking(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailProvidersWrite);

    let provider = state.provider_service.get(id).await.map_err(|e| match e {
        crate::EmailError::ProviderNotFound(_) => not_found().detail("Provider not found").build(),
        other => {
            error!("Failed to load email provider {id}: {other}");
            internal_server_error()
                .detail("Failed to load provider")
                .build()
        }
    })?;

    let webhook_url = tracking_webhook_url(&state).await?;

    let result = state
        .tracking_setup_service
        .setup_ses_event_tracking(id, &webhook_url)
        .await
        .map_err(|e| match e {
            crate::EmailError::Validation(msg) | crate::EmailError::Configuration(msg) => {
                bad_request().detail(msg).build()
            }
            crate::EmailError::ProviderNotFound(_) => {
                not_found().detail("Provider not found").build()
            }
            other => {
                error!("Event tracking setup failed: {}", other);
                temps_core::error_builder::ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                    .title("AWS Setup Failed")
                    .detail(other.to_string())
                    .build()
            }
        })?;

    let audit = EmailProviderTrackingSetupAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        provider_id: provider.id,
        name: provider.name,
        topic_arn: result.topic_arn.clone(),
        webhook_url: result.webhook_url.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(EmailTrackingSetupResponse {
        topic_arn: result.topic_arn,
        webhook_url: result.webhook_url,
        subscription_requested: result.subscription_requested,
        event_destination_attached: result.event_destination_attached,
    }))
}
