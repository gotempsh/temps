use crate::ConfigService;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::{problemdetails::Problem, AppSettings, LetsEncryptSettings, ScreenshotSettings};
use utoipa::{OpenApi, ToSchema};

pub struct SettingsState {
    pub config_service: Arc<ConfigService>,
}

/// Response for successful settings update
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SettingsUpdateResponse {
    pub message: String,
}

/// Safe response for application settings that masks sensitive fields
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppSettingsResponse {
    // Core settings
    pub external_url: Option<String>,
    pub preview_domain: String,

    // Screenshot settings
    pub screenshots: ScreenshotSettings,

    // TLS/ACME settings
    pub letsencrypt: LetsEncryptSettings,

    // DNS provider settings with masked API key
    pub dns_provider: DnsProviderSettingsMasked,
}

/// DNS provider settings with masked sensitive fields
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsProviderSettingsMasked {
    pub provider: String,
    pub cloudflare_api_key: Option<String>, // Will be masked as "******" if set
}

impl From<AppSettings> for AppSettingsResponse {
    fn from(settings: AppSettings) -> Self {
        Self {
            external_url: settings.external_url,
            preview_domain: settings.preview_domain,
            screenshots: settings.screenshots,
            letsencrypt: settings.letsencrypt,
            dns_provider: DnsProviderSettingsMasked {
                provider: settings.dns_provider.provider,
                // Mask the API key if it exists
                cloudflare_api_key: settings
                    .dns_provider
                    .cloudflare_api_key
                    .map(|_| "******".to_string()),
            },
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(get_settings, update_settings),
    components(schemas(
        AppSettings,
        AppSettingsResponse,
        DnsProviderSettingsMasked,
        SettingsUpdateResponse
    )),
    info(
        title = "Settings API",
        description = "API endpoints for managing application settings. \
        Provides configuration management for system-wide settings.",
        version = "1.0.0"
    )
)]
pub struct SettingsApiDoc;

pub fn configure_routes() -> Router<Arc<SettingsState>> {
    Router::new()
        .route("/settings", get(get_settings))
        .route("/settings", put(update_settings))
}

/// Get application settings
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings",
    responses(
        (status = 200, description = "Application settings with masked sensitive fields", body = AppSettingsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_settings(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    match app_state.config_service.get_settings().await {
        Ok(settings) => {
            // Convert to response type that masks sensitive fields
            let response = AppSettingsResponse::from(settings);
            Ok(Json(response))
        }
        Err(e) => {
            tracing::error!("Failed to get settings: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .type_("https://temps.sh/probs/settings-error")
                .title("Settings Error")
                .detail(format!("Failed to get settings: {}", e))
                .build())
        }
    }
}

/// Update application settings
#[utoipa::path(
    tag = "Settings",
    put,
    path = "/settings",
    request_body = AppSettings,
    responses(
        (status = 200, description = "Settings updated successfully", body = SettingsUpdateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 400, description = "Bad request - invalid settings"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn update_settings(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
    Json(mut settings): Json<AppSettings>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    // If cloudflare_api_key is "******", preserve the existing value
    if let Some(ref key) = settings.dns_provider.cloudflare_api_key {
        if key == "******" {
            // Get current settings to preserve the actual API key
            match app_state.config_service.get_settings().await {
                Ok(current_settings) => {
                    settings.dns_provider.cloudflare_api_key =
                        current_settings.dns_provider.cloudflare_api_key;
                }
                Err(e) => {
                    tracing::warn!(
                        "Could not fetch current settings to preserve API key: {}",
                        e
                    );
                }
            }
        }
    }

    match app_state.config_service.update_settings(settings).await {
        Ok(_) => Ok((
            StatusCode::OK,
            Json(SettingsUpdateResponse {
                message: "Settings updated successfully".to_string(),
            }),
        )),
        Err(e) => {
            tracing::error!("Failed to update settings: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .type_("https://temps.sh/probs/settings-error")
                .title("Settings Error")
                .detail(format!("Failed to update settings: {}", e))
                .build())
        }
    }
}
