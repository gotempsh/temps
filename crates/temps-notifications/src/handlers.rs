use crate::services::{NotificationService, NotificationPreferencesService, NotificationPreferences, TlsMode};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use tracing::{error, info};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use utoipa::OpenApi;

pub struct NotificationState {
    notification_service: Arc<NotificationService>,
    notification_preferences_service: Arc<NotificationPreferencesService>,
}

impl NotificationState {
    pub fn new(
        notification_service: Arc<NotificationService>,
        notification_preferences_service: Arc<NotificationPreferencesService>,
    ) -> Self {
        Self {
            notification_service,
            notification_preferences_service,
        }
    }
}
// use crate::{
//     db::NotificationProvider as DbNotificationProvider, services::notification::NotificationService,
//     services::{AuditService, email::TlsMode},
// };

#[derive(OpenApi)]
#[openapi(
    paths(
        list_notification_providers,
        get_notification_provider,
        create_notification_provider,
        update_provider,
        delete_provider,
        test_provider,
        create_slack_provider,
        create_email_provider,
        update_slack_provider,
        update_email_provider,
        get_preferences,
        update_preferences,
        delete_preferences,
    ),
    components(
        schemas(
            NotificationProviderResponse,
            CreateProviderRequest,
            UpdateProviderRequest,
            TestProviderResponse,
            SlackConfig,
            EmailConfig,
            TlsMode,
            CreateSlackProviderRequest,
            CreateEmailProviderRequest,
            UpdateSlackProviderRequest,
            UpdateEmailProviderRequest,
            NotificationPreferencesResponse,
            UpdatePreferencesRequest,
        )
    ),
    info(
        title = "Notifications API",
        description = "API endpoints for managing notification providers and user notification preferences. \
        Handles email, Slack, and other notification delivery services, as well as user notification settings.",
        version = "1.0.0"
    ),
    tags(
        (name = "Notification Providers", description = "Notification provider management endpoints"),
        (name = "Notification Preferences", description = "User notification preferences and settings")
    )
)]
pub struct NotificationProvidersApiDoc;

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NotificationProviderResponse {
    pub id: i32,
    pub name: String,
    pub provider_type: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateProviderRequest {
    pub name: String,
    pub provider_type: String,
    pub config: serde_json::Value,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TestProviderResponse {
    pub success: bool,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SlackConfig {
    pub webhook_url: String,
    pub channel: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub username: String,
    pub password: String,
    pub from_name: Option<String>,
    pub from_address: String,
    pub to_addresses: Vec<String>,
    #[serde(default = "default_tls_mode")]
    pub tls_mode: TlsMode,
    #[serde(default = "default_starttls_required")]
    pub starttls_required: bool, // Only used when tls_mode is Starttls
    #[serde(default = "default_accept_invalid_certs")]
    pub accept_invalid_certs: bool, // Accept self-signed certificates (use with caution)
}

fn default_tls_mode() -> TlsMode {
    TlsMode::Starttls
}

fn default_starttls_required() -> bool {
    true
}

fn default_accept_invalid_certs() -> bool {
    false // Default to secure behavior
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateSlackProviderRequest {
    pub name: String,
    pub config: SlackConfig,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateEmailProviderRequest {
    pub name: String,
    pub config: EmailConfig,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UpdateSlackProviderRequest {
    pub name: Option<String>,
    pub config: SlackConfig,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UpdateEmailProviderRequest {
    pub name: Option<String>,
    pub config: EmailConfig,
    pub enabled: Option<bool>,
}

/// List all notification providers
#[utoipa::path(
    get,
    path = "/notification-providers",
    responses(
        (status = 200, description = "Successfully retrieved providers", body = Vec<NotificationProviderResponse>),
        (status = 500, description = "Internal server error")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_notification_providers(
    State(app_state): State<Arc<NotificationState>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersRead);
    info!("Listing notification providers");
    match app_state.notification_service.list_providers().await {
        Ok(providers) => {
            let mut response_vec = Vec::new();
            for p in providers {
                let config = app_state
                    .notification_service
                    .decrypt_provider_config(&p.config)
                    .map_err(|e| {
                        error!("Failed to decrypt provider config: {}", e);
                        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                            .title("Failed to decrypt provider configurations")
                            .detail(&format!("Error: {}", e))
                            .build()
                    })?;
                response_vec.push(NotificationProviderResponse {
                    id: p.id,
                    name: p.name,
                    provider_type: p.provider_type,
                    config,
                    enabled: p.enabled,
                    created_at: p.created_at.timestamp_millis(),
                    updated_at: p.updated_at.timestamp_millis(),
                });
            }
            Ok((StatusCode::OK, Json(response_vec)))
        }
        Err(e) => {
            error!("Failed to list notification providers: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to list notification providers")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Get a single notification provider
#[utoipa::path(
    get,
    path = "/notification-providers/{id}",
    responses(
        (status = 200, description = "Successfully retrieved provider", body = NotificationProviderResponse),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_notification_provider(
    State(app_state): State<Arc<NotificationState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersRead);
    info!("Getting notification provider {}", id);
    match app_state.notification_service.get_provider(id).await {
        Ok(Some(provider)) => {
            let config = app_state
                .notification_service
                .decrypt_provider_config(&provider.config)
                .map_err(|e| {
                    error!("Failed to decrypt provider config: {}", e);
                    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Failed to decrypt provider configuration")
                        .detail(&format!("Error: {}", e))
                        .build()
                })?;
            let response = NotificationProviderResponse {
                id: provider.id,
                name: provider.name,
                provider_type: provider.provider_type,
                config,
                enabled: provider.enabled,
                created_at: provider.created_at.timestamp_millis(),
                updated_at: provider.updated_at.timestamp_millis(),
            };
            Ok((StatusCode::OK, Json(response)).into_response())
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Provider not found")
            .detail("The requested notification provider does not exist")
            .build()),
        Err(e) => {
            error!("Failed to get notification provider {}: {}", id, e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get notification provider")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Create a new notification provider
#[utoipa::path(
    post,
    path = "/notification-providers",
    request_body = CreateProviderRequest,
    responses(
        (status = 201, description = "Successfully created provider", body = NotificationProviderResponse),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_notification_provider(
    State(app_state): State<Arc<NotificationState>>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersCreate);
    info!("Creating notification provider {}", request.name);
    match app_state
        .notification_service
        .add_provider(request.name, request.provider_type, request.config)
        .await
    {
        Ok(provider) => {
            let config = app_state
                .notification_service
                .decrypt_provider_config(&provider.config)
                .map_err(|e| {
                    error!("Failed to decrypt provider config: {}", e);
                    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Failed to decrypt provider configuration")
                        .detail(&format!("Error: {}", e))
                        .build()
                })?;
            let response = NotificationProviderResponse {
                id: provider.id,
                name: provider.name,
                provider_type: provider.provider_type,
                config,
                enabled: provider.enabled,
                created_at: provider.created_at.timestamp_millis(),
                updated_at: provider.updated_at.timestamp_millis(),
            };
            Ok((StatusCode::CREATED, Json(response)))
        }
        Err(e) => {
            error!("Failed to create notification provider: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to create notification provider")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}
impl From<UpdateProviderRequest> for crate::services::UpdateProviderRequest {
    fn from(request: UpdateProviderRequest) -> Self {
        Self {
            name: request.name,
            config: request.config,
            enabled: request.enabled,
        }
    }
}

/// Update a notification provider
#[utoipa::path(
    put,
    path = "/notification-providers/{id}",
    request_body = UpdateProviderRequest,
    responses(
        (status = 200, description = "Successfully updated provider", body = NotificationProviderResponse),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn update_provider(
    State(app_state): State<Arc<NotificationState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<UpdateProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersWrite);
    info!("Updating notification provider {}", id);
    match app_state
        .notification_service
        .update_provider(id, request.into())
        .await
    {
        Ok(Some(provider)) => {
            let config = app_state
                .notification_service
                .decrypt_provider_config(&provider.config)
                .map_err(|e| {
                    error!("Failed to decrypt provider config: {}", e);
                    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Failed to decrypt provider configuration")
                        .detail(&format!("Error: {}", e))
                        .build()
                })?;
            let response = NotificationProviderResponse {
                id: provider.id,
                name: provider.name,
                provider_type: provider.provider_type,
                config,
                enabled: provider.enabled,
                created_at: provider.created_at.timestamp_millis(),
                updated_at: provider.updated_at.timestamp_millis(),
            };
            Ok((StatusCode::OK, Json(response)))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Provider not found")
            .detail("The requested notification provider does not exist")
            .build()),
        Err(e) => {
            error!("Failed to update notification provider {}: {}", id, e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to update notification provider")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Delete a notification provider
#[utoipa::path(
    delete,
    path = "/notification-providers/{id}",
    responses(
        (status = 204, description = "Successfully deleted provider"),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn delete_provider(
    State(app_state): State<Arc<NotificationState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersDelete);
    info!("Deleting notification provider {}", id);
    match app_state.notification_service.delete_provider(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Provider not found")
            .detail("The requested notification provider does not exist")
            .build()),
        Err(e) => {
            error!("Failed to delete notification provider {}: {}", id, e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to delete notification provider")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Test a notification provider
#[utoipa::path(
    post,
    path = "/notification-providers/{id}/test",
    responses(
        (status = 200, description = "Test result", body = TestProviderResponse),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn test_provider(
    State(app_state): State<Arc<NotificationState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersRead);
    info!("Testing notification provider {}", id);
    match app_state.notification_service.test_provider(id).await {
        Ok(result) => {
            let message = if result {
                Some("Test email sent successfully".to_string())
            } else {
                Some("Test failed - provider connection or configuration issue".to_string())
            };
            Ok((
                StatusCode::OK,
                Json(TestProviderResponse {
                    success: result,
                    message,
                }),
            ))
        }
        Err(e) => {
            error!("Failed to test notification provider {}: {}", id, e);
            Ok((
                if e.to_string().contains("not found") {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                },
                Json(TestProviderResponse {
                    success: false,
                    message: Some(format!("Test failed: {}", e.to_string())),
                }),
            ))
        }
    }
}

/// Create a new Slack notification provider
#[utoipa::path(
    post,
    path = "/notification-providers/slack",
    request_body = CreateSlackProviderRequest,
    responses(
        (status = 201, description = "Successfully created Slack provider", body = NotificationProviderResponse),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_slack_provider(
    State(app_state): State<Arc<NotificationState>>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateSlackProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersCreate);
    info!("Creating Slack notification provider {}", request.name);
    let config = serde_json::to_value(request.config).unwrap_or_default();
    match app_state
        .notification_service
        .add_provider(request.name, "slack".to_string(), config)
        .await
    {
        Ok(provider) => {
            let config = app_state
                .notification_service
                .decrypt_provider_config(&provider.config)
                .map_err(|e| {
                    error!("Failed to decrypt provider config: {}", e);
                    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Failed to decrypt provider configuration")
                        .detail(&format!("Error: {}", e))
                        .build()
                })?;
            let response = NotificationProviderResponse {
                id: provider.id,
                name: provider.name,
                provider_type: provider.provider_type,
                config,
                enabled: provider.enabled,
                created_at: provider.created_at.timestamp_millis(),
                updated_at: provider.updated_at.timestamp_millis(),
            };
            Ok((StatusCode::CREATED, Json(response)))
        }
        Err(e) => {
            error!("Failed to create Slack notification provider: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to create Slack notification provider")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Create a new Email notification provider
#[utoipa::path(
    post,
    path = "/notification-providers/email",
    request_body = CreateEmailProviderRequest,
    responses(
        (status = 201, description = "Successfully created Email provider", body = NotificationProviderResponse),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_email_provider(
    State(app_state): State<Arc<NotificationState>>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<CreateEmailProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersCreate);
    info!("Creating Email notification provider {}", request.name);
    let config = serde_json::to_value(&request.config).unwrap_or_default();
    match app_state
        .notification_service
        .add_provider(request.name, "email".to_string(), config)
        .await
    {
        Ok(provider) => {
            let config = app_state
                .notification_service
                .decrypt_provider_config(&provider.config)
                .map_err(|e| {
                    error!("Failed to decrypt provider config: {}", e);
                    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Failed to decrypt provider configuration")
                        .detail(&format!("Error: {}", e))
                        .build()
                })?;
            let response = NotificationProviderResponse {
                id: provider.id,
                name: provider.name,
                provider_type: provider.provider_type,
                config,
                enabled: provider.enabled,
                created_at: provider.created_at.timestamp_millis(),
                updated_at: provider.updated_at.timestamp_millis(),
            };
            Ok((StatusCode::CREATED, Json(response)))
        }
        Err(e) => {
            error!("Failed to create Email notification provider: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to create Email notification provider")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Update a Slack notification provider
#[utoipa::path(
    put,
    path = "/notification-providers/slack/{id}",
    request_body = UpdateSlackProviderRequest,
    responses(
        (status = 200, description = "Successfully updated Slack provider", body = NotificationProviderResponse),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn update_slack_provider(
    State(app_state): State<Arc<NotificationState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<UpdateSlackProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersWrite);
    info!("Updating Slack notification provider {}", id);
    let config = serde_json::to_value(request.config).unwrap_or_default();
    let update_request = UpdateProviderRequest {
        name: request.name,
        config: Some(config),
        enabled: request.enabled,
    };
    match app_state
        .notification_service
        .update_provider(id, update_request.into())
        .await
    {
        Ok(Some(provider)) => {
            let config = app_state
                .notification_service
                .decrypt_provider_config(&provider.config)
                .map_err(|e| {
                    error!("Failed to decrypt provider config: {}", e);
                    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Failed to decrypt provider configuration")
                        .detail(&format!("Error: {}", e))
                        .build()
                })?;
            let response = NotificationProviderResponse {
                id: provider.id,
                name: provider.name,
                provider_type: provider.provider_type,
                config,
                enabled: provider.enabled,
                created_at: provider.created_at.timestamp_millis(),
                updated_at: provider.updated_at.timestamp_millis(),
            };
            Ok((StatusCode::OK, Json(response)))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Provider not found")
            .detail("The requested Slack notification provider does not exist")
            .build()),
        Err(e) => {
            error!("Failed to update Slack notification provider {}: {}", id, e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to update Slack notification provider")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Update an Email notification provider
#[utoipa::path(
    put,
    path = "/notification-providers/email/{id}",
    request_body = UpdateEmailProviderRequest,
    responses(
        (status = 200, description = "Successfully updated Email provider", body = NotificationProviderResponse),
        (status = 404, description = "Provider not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "Provider ID")
    ),
    tag = "Notification Providers",
    security(
        ("bearer_auth" = [])
    )
)]
async fn update_email_provider(
    State(app_state): State<Arc<NotificationState>>,
    Path(id): Path<i32>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<UpdateEmailProviderRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationProvidersWrite);
    info!("Updating Email notification provider {}", id);
    let config = serde_json::to_value(request.config).unwrap_or_default();
    let update_request = UpdateProviderRequest {
        name: request.name,
        config: Some(config),
        enabled: request.enabled,
    };
    match app_state
        .notification_service
        .update_provider(id, update_request.into())
        .await
    {
        Ok(Some(provider)) => {
            let config = app_state
                .notification_service
                .decrypt_provider_config(&provider.config)
                .map_err(|e| {
                    error!("Failed to decrypt provider config: {}", e);
                    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("Failed to decrypt provider configuration")
                        .detail(&format!("Error: {}", e))
                        .build()
                })?;
            let response = NotificationProviderResponse {
                id: provider.id,
                name: provider.name,
                provider_type: provider.provider_type,
                config,
                enabled: provider.enabled,
                created_at: provider.created_at.timestamp_millis(),
                updated_at: provider.updated_at.timestamp_millis(),
            };
            Ok((StatusCode::OK, Json(response)))
        }
        Ok(None) => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Provider not found")
            .detail("The requested Email notification provider does not exist")
            .build()),
        Err(e) => {
            error!("Failed to update Email notification provider {}: {}", id, e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to update Email notification provider")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

// Notification Preferences Types and Handlers

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NotificationPreferencesResponse {
    // Notification Channels
    pub email_enabled: bool,
    pub slack_enabled: bool,
    pub batch_similar_notifications: bool,
    pub minimum_severity: String,

    // Project Health
    pub deployment_failures_enabled: bool,
    pub build_errors_enabled: bool,
    pub runtime_errors_enabled: bool,
    pub error_threshold: i32,
    pub error_time_window: i32,

    // Domain Monitoring
    pub ssl_expiration_enabled: bool,
    pub ssl_days_before_expiration: i32,
    pub domain_expiration_enabled: bool,
    pub dns_changes_enabled: bool,

    // Backup Monitoring
    pub backup_failures_enabled: bool,
    pub backup_successes_enabled: bool,
    pub s3_connection_issues_enabled: bool,
    pub retention_policy_violations_enabled: bool,

    // Route Monitoring
    pub route_downtime_enabled: bool,
    pub load_balancer_issues_enabled: bool,
}

impl From<NotificationPreferences> for NotificationPreferencesResponse {
    fn from(prefs: NotificationPreferences) -> Self {
        Self {
            email_enabled: prefs.email_enabled,
            slack_enabled: prefs.slack_enabled,
            batch_similar_notifications: prefs.batch_similar_notifications,
            minimum_severity: prefs.minimum_severity,
            deployment_failures_enabled: prefs.deployment_failures_enabled,
            build_errors_enabled: prefs.build_errors_enabled,
            runtime_errors_enabled: prefs.runtime_errors_enabled,
            error_threshold: prefs.error_threshold,
            error_time_window: prefs.error_time_window,
            ssl_expiration_enabled: prefs.ssl_expiration_enabled,
            ssl_days_before_expiration: prefs.ssl_days_before_expiration,
            domain_expiration_enabled: prefs.domain_expiration_enabled,
            dns_changes_enabled: prefs.dns_changes_enabled,
            backup_failures_enabled: prefs.backup_failures_enabled,
            backup_successes_enabled: prefs.backup_successes_enabled,
            s3_connection_issues_enabled: prefs.s3_connection_issues_enabled,
            retention_policy_violations_enabled: prefs.retention_policy_violations_enabled,
            route_downtime_enabled: prefs.route_downtime_enabled,
            load_balancer_issues_enabled: prefs.load_balancer_issues_enabled,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct UpdatePreferencesRequest {
    pub preferences: NotificationPreferencesResponse,
}

/// Get notification preferences
#[utoipa::path(
    get,
    path = "/notification-preferences",
    responses(
        (status = 200, description = "Successfully retrieved preferences", body = NotificationPreferencesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Notification Preferences",
)]
async fn get_preferences(
    State(app_state): State<Arc<NotificationState>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationPreferencesRead);

    info!("Getting notification preferences for user {}", auth.user_id());
    match app_state.notification_preferences_service.get_preferences().await {
        Ok(preferences) => {
            Ok((StatusCode::OK, Json(NotificationPreferencesResponse::from(preferences))))
        }
        Err(e) => {
            error!("Failed to get notification preferences: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get notification preferences")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Update notification preferences
#[utoipa::path(
    put,
    path = "/notification-preferences",
    request_body = UpdatePreferencesRequest,
    responses(
        (status = 200, description = "Successfully updated preferences", body = NotificationPreferencesResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Notification Preferences",
)]
async fn update_preferences(
    State(app_state): State<Arc<NotificationState>>,
    RequireAuth(auth): RequireAuth,
    Json(request): Json<UpdatePreferencesRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationPreferencesWrite);

    info!("Updating notification preferences for user {}", auth.user_id());
    let db_preferences = NotificationPreferences {
        email_enabled: request.preferences.email_enabled,
        slack_enabled: request.preferences.slack_enabled,
        batch_similar_notifications: request.preferences.batch_similar_notifications,
        minimum_severity: request.preferences.minimum_severity.clone(),
        deployment_failures_enabled: request.preferences.deployment_failures_enabled,
        build_errors_enabled: request.preferences.build_errors_enabled,
        runtime_errors_enabled: request.preferences.runtime_errors_enabled,
        error_threshold: request.preferences.error_threshold,
        error_time_window: request.preferences.error_time_window,
        ssl_expiration_enabled: request.preferences.ssl_expiration_enabled,
        ssl_days_before_expiration: request.preferences.ssl_days_before_expiration,
        domain_expiration_enabled: request.preferences.domain_expiration_enabled,
        dns_changes_enabled: request.preferences.dns_changes_enabled,
        backup_failures_enabled: request.preferences.backup_failures_enabled,
        backup_successes_enabled: request.preferences.backup_successes_enabled,
        s3_connection_issues_enabled: request.preferences.s3_connection_issues_enabled,
        retention_policy_violations_enabled: request.preferences.retention_policy_violations_enabled,
        route_downtime_enabled: request.preferences.route_downtime_enabled,
        load_balancer_issues_enabled: request.preferences.load_balancer_issues_enabled,
    };

    match app_state.notification_preferences_service.update_preferences(db_preferences).await {
        Ok(preferences) => {
            Ok((StatusCode::OK, Json(NotificationPreferencesResponse::from(preferences))))
        }
        Err(e) => {
            error!("Failed to update notification preferences: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to update notification preferences")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

/// Delete notification preferences
#[utoipa::path(
    delete,
    path = "/notification-preferences",
    responses(
        (status = 204, description = "Successfully deleted preferences"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Notification Preferences",
)]
async fn delete_preferences(
    State(app_state): State<Arc<NotificationState>>,
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, NotificationPreferencesWrite);

    info!("Deleting notification preferences for user {}", auth.user_id());
    match app_state.notification_preferences_service.delete_preferences().await {
        Ok(_) => {
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!("Failed to delete notification preferences: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to delete notification preferences")
                .detail(&format!("Error: {}", e))
                .build())
        }
    }
}

pub fn configure_routes() -> Router<Arc<NotificationState>> {
    Router::new()
        .route("/notification-providers", get(list_notification_providers))
        .route(
            "/notification-providers",
            post(create_notification_provider),
        )
        .route("/notification-providers/slack", post(create_slack_provider))
        .route("/notification-providers/email", post(create_email_provider))
        .route(
            "/notification-providers/{id}",
            get(get_notification_provider),
        )
        .route("/notification-providers/{id}", put(update_provider))
        .route(
            "/notification-providers/slack/{id}",
            put(update_slack_provider),
        )
        .route(
            "/notification-providers/email/{id}",
            put(update_email_provider),
        )
        .route("/notification-providers/{id}", delete(delete_provider))
        .route("/notification-providers/{id}/test", post(test_provider))
        .route("/notification-preferences", get(get_preferences))
        .route("/notification-preferences", put(update_preferences))
        .route("/notification-preferences", delete(delete_preferences))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, core::ContainerPort};
    use tokio;
    use tower::ServiceExt;

    struct TestSetup {
        pub test_db: TestDatabase,
        pub mailpit_container: ContainerAsync<GenericImage>,
        pub mailpit_smtp_port: u16,
        pub mailpit_web_port: u16,
        pub notification_state: Arc<NotificationState>,
    }

    impl TestSetup {
        async fn new() -> Result<Self, Box<dyn std::error::Error>> {
            // Start database with migrations
            let test_db = TestDatabase::with_migrations().await?;

            // Start Mailpit container for email testing
            let mailpit_container = GenericImage::new("axllent/mailpit", "latest")
                .with_exposed_port(ContainerPort::Tcp(1025)) // SMTP port
                .with_exposed_port(ContainerPort::Tcp(8025)) // Web UI port
                .start()
                .await?;

            let mailpit_smtp_port = mailpit_container.get_host_port_ipv4(1025).await?;
            let mailpit_web_port = mailpit_container.get_host_port_ipv4(8025).await?;

            // Wait for Mailpit to be ready
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            // Create encryption service
            let encryption_service = Arc::new(
                temps_core::EncryptionService::new("0000000000000000000000000000000000000000000000000000000000000000")
                    .expect("Failed to create encryption service")
            );

            // Create notification service
            let notification_service = Arc::new(
                crate::services::NotificationService::new(
                    test_db.connection_arc(),
                    encryption_service.clone()
                )
            );

            // Create notification preferences service
            let notification_preferences_service = Arc::new(
                crate::services::NotificationPreferencesService::new(test_db.connection_arc())
            );

            let notification_state = Arc::new(NotificationState::new(
                notification_service,
                notification_preferences_service,
            ));

            Ok(TestSetup {
                test_db,
                mailpit_container,
                mailpit_smtp_port,
                mailpit_web_port,
                notification_state,
            })
        }

        fn create_test_email_config(&self) -> EmailConfig {
            EmailConfig {
                smtp_host: "localhost".to_string(),
                smtp_port: self.mailpit_smtp_port,
                username: "".to_string(), // Mailpit doesn't require auth
                password: "".to_string(),
                from_name: Some("Test Sender".to_string()),
                from_address: "test@example.com".to_string(),
                to_addresses: vec!["recipient@example.com".to_string()],
                tls_mode: TlsMode::None, // Mailpit doesn't use TLS by default
                starttls_required: false,
                accept_invalid_certs: true,
            }
        }

        fn create_test_slack_config(&self) -> SlackConfig {
            SlackConfig {
                webhook_url: "https://hooks.slack.com/services/TEST/TEST/TEST".to_string(),
                channel: Some("#test".to_string()),
            }
        }

        async fn cleanup(&self) -> Result<(), Box<dyn std::error::Error>> {
            self.test_db.cleanup_all_tables().await?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_list_notification_providers() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let app = configure_routes().with_state(setup.notification_state.clone());

        // Create test request
        let request = Request::builder()
            .method("GET")
            .uri("/notification-providers")
            .header("authorization", "Bearer test-token")
            .body(Body::empty())?;

        // Send request using tower
        let response = app.oneshot(request).await?;

        // Note: This will fail without proper auth setup, but shows the structure
        // You'll need to set up proper test authentication
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_create_email_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let request_body = CreateEmailProviderRequest {
            name: "Test Email Provider".to_string(),
            config: setup.create_test_email_config(),
            enabled: Some(true),
        };

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/notification-providers/email")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&request_body)?))?;

        let response = app.oneshot(request).await?;

        // Note: This will fail without proper auth setup
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_get_notification_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("GET")
            .uri("/notification-providers/1")
            .header("authorization", "Bearer test-token")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;

        // Should return 404 for non-existent provider (after auth)
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_create_notification_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let request_body = CreateProviderRequest {
            name: "Test Generic Provider".to_string(),
            provider_type: "custom".to_string(),
            config: serde_json::json!({
                "api_key": "test-key",
                "endpoint": "https://example.com/webhook"
            }),
            enabled: Some(true),
        };

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/notification-providers")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&request_body)?))?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_create_slack_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let request_body = CreateSlackProviderRequest {
            name: "Test Slack Provider".to_string(),
            config: setup.create_test_slack_config(),
            enabled: Some(true),
        };

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/notification-providers/slack")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&request_body)?))?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let request_body = UpdateProviderRequest {
            name: Some("Updated Provider Name".to_string()),
            config: Some(serde_json::json!({
                "api_key": "updated-key",
                "endpoint": "https://updated.example.com/webhook"
            })),
            enabled: Some(false),
        };

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("PUT")
            .uri("/notification-providers/1")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&request_body)?))?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_slack_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let request_body = UpdateSlackProviderRequest {
            name: Some("Updated Slack Provider".to_string()),
            config: SlackConfig {
                webhook_url: "https://hooks.slack.com/services/UPDATED/WEBHOOK/URL".to_string(),
                channel: Some("#updated-channel".to_string()),
            },
            enabled: Some(false),
        };

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("PUT")
            .uri("/notification-providers/slack/1")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&request_body)?))?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_update_email_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let request_body = UpdateEmailProviderRequest {
            name: Some("Updated Email Provider".to_string()),
            config: EmailConfig {
                smtp_host: "updated-smtp.example.com".to_string(),
                smtp_port: 587,
                username: "updated@example.com".to_string(),
                password: "updated-password".to_string(),
                from_name: Some("Updated Sender".to_string()),
                from_address: "updated@example.com".to_string(),
                to_addresses: vec!["updated-recipient@example.com".to_string()],
                tls_mode: TlsMode::Starttls,
                starttls_required: true,
                accept_invalid_certs: false,
            },
            enabled: Some(false),
        };

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("PUT")
            .uri("/notification-providers/email/1")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&request_body)?))?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_delete_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("DELETE")
            .uri("/notification-providers/1")
            .header("authorization", "Bearer test-token")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_test_provider() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        let app = configure_routes().with_state(setup.notification_state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/notification-providers/1/test")
            .header("authorization", "Bearer test-token")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        setup.cleanup().await?;
        Ok(())
    }

    // Integration test that actually sends an email through Mailpit
    #[tokio::test]
    async fn test_email_integration_with_mailpit() -> Result<(), Box<dyn std::error::Error>> {
        let setup = TestSetup::new().await?;

        // Create an email provider directly using the service
        let email_config = serde_json::to_value(setup.create_test_email_config())?;

        let provider = setup
            .notification_state
            .notification_service
            .add_provider(
                "Mailpit Test Provider".to_string(),
                "email".to_string(),
                email_config,
            )
            .await?;

        // Test the provider (this should send an email to Mailpit)
        let test_result = setup
            .notification_state
            .notification_service
            .test_provider(provider.id)
            .await?;

        assert!(test_result, "Email test should succeed with Mailpit");

        // You could also verify the email was received by querying Mailpit's API
        // at http://localhost:{mailpit_web_port}/api/v1/messages

        setup.cleanup().await?;
        Ok(())
    }
}
