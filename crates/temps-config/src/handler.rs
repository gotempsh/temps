use crate::ConfigService;
use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::{
    problemdetails::Problem, AppSettings, AuditContext, AuditLogger, AuditOperation,
    ContainerLogSettings, DiskSpaceAlertSettings, LetsEncryptSettings, RateLimitSettings,
    RequestMetadata, ScreenshotSettings, SecurityHeadersSettings,
};
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

pub struct SettingsState {
    pub config_service: Arc<ConfigService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub route_table_refresher: Option<Arc<dyn temps_core::route_table::RouteTableRefresher>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SettingsUpdatedAudit {
    context: AuditContext,
}

impl AuditOperation for SettingsUpdatedAudit {
    fn operation_type(&self) -> String {
        "SETTINGS_UPDATED".to_string()
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

/// Response for successful settings update
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SettingsUpdateResponse {
    pub message: String,
}

/// Response returned when a join token is generated (plaintext shown once)
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GenerateJoinTokenResponse {
    /// The plaintext join token — shown only once, save it now
    pub token: String,
    pub message: String,
}

/// Response for join token status check
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct JoinTokenStatusResponse {
    /// Whether a join token has been configured
    pub has_token: bool,
}

/// Public settings response containing only non-sensitive feature flags
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PublicSettingsResponse {
    /// Whether demo mode is enabled
    pub demo_enabled: bool,
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

    // Security settings
    pub security_headers: SecurityHeadersSettings,
    pub rate_limiting: RateLimitSettings,

    // Docker registry settings with masked password
    pub docker_registry: DockerRegistrySettingsMasked,

    // Monitoring settings
    pub disk_space_alert: DiskSpaceAlertSettings,

    // Docker container log rotation settings
    pub container_logs: ContainerLogSettings,
}

/// DNS provider settings with masked sensitive fields
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsProviderSettingsMasked {
    pub provider: String,
    pub cloudflare_api_key: Option<String>, // Will be masked as "******" if set
}

/// Docker registry settings with masked sensitive fields
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DockerRegistrySettingsMasked {
    pub enabled: bool,
    pub registry_url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>, // Will be masked as "******" if set
    pub tls_verify: bool,
    pub ca_certificate: Option<String>,
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
            security_headers: settings.security_headers,
            rate_limiting: settings.rate_limiting,
            docker_registry: DockerRegistrySettingsMasked {
                enabled: settings.docker_registry.enabled,
                registry_url: settings.docker_registry.registry_url,
                username: settings.docker_registry.username,
                // Mask the password if it exists
                password: settings
                    .docker_registry
                    .password
                    .map(|_| "******".to_string()),
                tls_verify: settings.docker_registry.tls_verify,
                ca_certificate: settings.docker_registry.ca_certificate,
            },
            disk_space_alert: settings.disk_space_alert,
            container_logs: settings.container_logs,
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_public_settings,
        get_settings,
        update_settings,
        generate_join_token,
        revoke_join_token,
        get_join_token_status,
        refresh_route_table,
    ),
    components(schemas(
        AppSettings,
        AppSettingsResponse,
        ContainerLogSettings,
        DnsProviderSettingsMasked,
        DockerRegistrySettingsMasked,
        PublicSettingsResponse,
        SettingsUpdateResponse,
        GenerateJoinTokenResponse,
        JoinTokenStatusResponse,
        RouteRefreshResponse,
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
        .route("/settings/public", get(get_public_settings))
        .route("/settings", get(get_settings))
        .route("/settings", put(update_settings))
        .route("/settings/join-token/generate", post(generate_join_token))
        .route("/settings/join-token", delete(revoke_join_token))
        .route("/settings/join-token/status", get(get_join_token_status))
        .route("/settings/routes/refresh", post(refresh_route_table))
}

/// Get public settings (no authentication required)
///
/// Returns non-sensitive feature flags like demo mode status.
/// This endpoint is intentionally unauthenticated so the login page can use it.
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/public",
    responses(
        (status = 200, description = "Public settings", body = PublicSettingsResponse),
        (status = 500, description = "Internal server error")
    )
)]
async fn get_public_settings(
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    match app_state.config_service.get_settings().await {
        Ok(settings) => Ok(Json(PublicSettingsResponse {
            demo_enabled: settings.demo_mode.enabled,
        })),
        Err(e) => {
            tracing::error!("Failed to get public settings: {}", e);
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .type_("https://temps.sh/probs/settings-error")
                .title("Settings Error")
                .detail("Failed to get public settings".to_string())
                .build())
        }
    }
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
    Extension(metadata): Extension<RequestMetadata>,
    Json(mut settings): Json<AppSettings>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    // If sensitive fields are masked, preserve the existing values
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

    // If docker registry password is "******", preserve the existing value
    if let Some(ref password) = settings.docker_registry.password {
        if password == "******" {
            // Get current settings to preserve the actual password
            match app_state.config_service.get_settings().await {
                Ok(current_settings) => {
                    settings.docker_registry.password = current_settings.docker_registry.password;
                }
                Err(e) => {
                    tracing::warn!(
                        "Could not fetch current settings to preserve Docker registry password: {}",
                        e
                    );
                }
            }
        }
    }

    // Validate and sanitize external_url
    if let Some(ref mut ext_url) = settings.external_url {
        *ext_url = ext_url.trim().to_string();
        *ext_url = ext_url.trim_end_matches('/').to_string();
        if !ext_url.starts_with("http://") && !ext_url.starts_with("https://") {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .detail("External URL must start with http:// or https://")
                .build());
        }
        if ext_url.contains('#') || ext_url.contains('?') {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .detail("External URL must not contain '#' or '?' characters")
                .build());
        }
        if url::Url::parse(ext_url).is_err() {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .detail("External URL is not a valid URL")
                .build());
        }
    }

    match app_state.config_service.update_settings(settings).await {
        Ok(_) => {
            let audit = SettingsUpdatedAudit {
                context: AuditContext {
                    user_id: auth.user_id(),
                    ip_address: Some(metadata.ip_address.clone()),
                    user_agent: metadata.user_agent.clone(),
                },
            };
            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!("Failed to create audit log: {}", e);
            }

            Ok((
                StatusCode::OK,
                Json(SettingsUpdateResponse {
                    message: "Settings updated successfully".to_string(),
                }),
            ))
        }
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

/// SHA-256 hash a token string
fn sha256_hash(token: &str) -> String {
    let digest = sha2::Sha256::digest(token.as_bytes());
    format!("{:x}", digest)
}

#[derive(Debug, Clone, serde::Serialize)]
struct JoinTokenGeneratedAudit {
    context: AuditContext,
}

impl AuditOperation for JoinTokenGeneratedAudit {
    fn operation_type(&self) -> String {
        "JOIN_TOKEN_GENERATED".to_string()
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

#[derive(Debug, Clone, serde::Serialize)]
struct JoinTokenRevokedAudit {
    context: AuditContext,
}

impl AuditOperation for JoinTokenRevokedAudit {
    fn operation_type(&self) -> String {
        "JOIN_TOKEN_REVOKED".to_string()
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

/// Generate a new join token for multi-node cluster registration
///
/// Creates a random 32-byte hex token, stores the SHA-256 hash in settings,
/// and returns the plaintext exactly once. If a token already exists, it is replaced.
#[utoipa::path(
    tag = "Settings",
    post,
    path = "/settings/join-token/generate",
    responses(
        (status = 200, description = "Join token generated", body = GenerateJoinTokenResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn generate_join_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    // Generate a random 32-byte token as hex
    let plaintext_token = {
        let mut rng = rand::thread_rng();
        let bytes: Vec<u8> = (0..32).map(|_| rng.gen::<u8>()).collect();
        hex::encode(bytes)
    };
    let token_hash = sha256_hash(&plaintext_token);

    // Store the hash in settings
    app_state
        .config_service
        .update_setting_field(|s| {
            s.multi_node.join_token_hash = Some(token_hash);
        })
        .await
        .map_err(|e| {
            error!("Failed to store join token hash: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Settings Error")
                .detail(format!("Failed to generate join token: {}", e))
                .build()
        })?;

    info!(user_id = auth.user_id(), "Join token generated");

    let audit = JoinTokenGeneratedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(GenerateJoinTokenResponse {
        token: plaintext_token,
        message: "Join token generated. Save this token — it will not be shown again.".to_string(),
    }))
}

/// Revoke the current join token
///
/// Removes the stored join token hash, allowing any node to register
/// (if no other authentication is in place).
#[utoipa::path(
    tag = "Settings",
    delete,
    path = "/settings/join-token",
    responses(
        (status = 200, description = "Join token revoked", body = SettingsUpdateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn revoke_join_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    app_state
        .config_service
        .update_setting_field(|s| {
            s.multi_node.join_token_hash = None;
        })
        .await
        .map_err(|e| {
            error!("Failed to revoke join token: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Settings Error")
                .detail(format!("Failed to revoke join token: {}", e))
                .build()
        })?;

    info!(user_id = auth.user_id(), "Join token revoked");

    let audit = JoinTokenRevokedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(SettingsUpdateResponse {
        message: "Join token revoked successfully".to_string(),
    }))
}

/// Check whether a join token is currently configured
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/join-token/status",
    responses(
        (status = 200, description = "Join token status", body = JoinTokenStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_join_token_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let settings = app_state.config_service.get_settings().await.map_err(|e| {
        error!("Failed to read settings for join token status: {}", e);
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Settings Error")
            .detail(format!("Failed to check join token status: {}", e))
            .build()
    })?;

    Ok(Json(JoinTokenStatusResponse {
        has_token: settings.multi_node.join_token_hash.is_some(),
    }))
}

#[derive(Debug, Serialize, ToSchema)]
struct RouteRefreshResponse {
    /// Number of routes loaded
    route_count: usize,
    /// Human-readable message
    message: String,
}

/// Manually refresh the proxy route table
///
/// Reloads all routes from the database into the in-memory proxy cache.
/// Useful as a workaround when routes are out of sync.
#[utoipa::path(
    tag = "Settings",
    post,
    path = "/settings/routes/refresh",
    responses(
        (status = 200, description = "Route table refreshed", body = RouteRefreshResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn refresh_route_table(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let refresher = app_state.route_table_refresher.as_ref().ok_or_else(|| {
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Route Table Unavailable")
            .detail("Route table refresher is not configured")
            .build()
    })?;

    let route_count = refresher.refresh_routes().await.map_err(|e| {
        error!("Failed to refresh route table: {}", e);
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Route Refresh Failed")
            .detail(format!("Failed to refresh route table: {}", e))
            .build()
    })?;

    info!(
        "Route table manually refreshed by user {} ({} routes loaded)",
        auth.user_id(),
        route_count
    );

    Ok(Json(RouteRefreshResponse {
        route_count,
        message: format!(
            "Route table refreshed successfully ({} routes loaded)",
            route_count
        ),
    }))
}
