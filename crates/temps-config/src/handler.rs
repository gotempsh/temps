use crate::disk_status::DiskSpaceCheckResult;
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
    problemdetails::Problem, AiConfigSettings, AppSettings, AuditContext, AuditLogger,
    AuditOperation, ClusterDnsSettings, ContainerLogSettings, DiskSpaceAlertSettings,
    LetsEncryptSettings, MetricsStoreKind, RateLimitSettings, RequestMetadata, ScreenshotSettings,
    SecurityHeadersSettings,
};
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

pub struct SettingsState {
    pub config_service: Arc<ConfigService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub route_table_refresher: Option<Arc<dyn temps_core::route_table::RouteTableRefresher>>,
    /// Node enrollment token minting/listing/revocation (ADR-020 WS-1.1).
    pub enrollment_token_service: Arc<crate::enrollment_tokens::EnrollmentTokenService>,
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

/// Safe response for application settings that masks sensitive fields
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppSettingsResponse {
    // Core settings
    pub external_url: Option<String>,
    pub internal_url: Option<String>,
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

    // Agent sandbox settings with masked per-provider credentials
    pub agent_sandbox: AgentSandboxSettingsMasked,

    // AI config (config repo for skills/MCP/etc)
    pub ai_config: AiConfigSettings,

    // Workspace preview gateway (shared_secret masked)
    pub preview_gateway: PreviewGatewaySettingsMasked,

    // Multi-node cluster settings (join_token_hash elided)
    pub multi_node: MultiNodeSettingsMasked,

    // Metrics monitoring settings (clickhouse_url masked)
    pub monitoring: MonitoringSettingsMasked,

    /// The storage backend the runtime is **actually** using for metrics,
    /// after reconciling the `monitoring.store` toggle with the server's
    /// `TEMPS_CLICKHOUSE_*` configuration. When `monitoring.store` is
    /// `click_house` but those env vars are not fully set, the runtime falls
    /// back to TimescaleDB — in that case this reports `timescale_db` even
    /// though `monitoring.store` says `click_house`. The UI shows this as the
    /// effective backend and warns when it diverges from the configured store.
    pub effective_metrics_store: MetricsStoreKind,

    // Outbound TLS verification toggle
    pub insecure_tls: bool,

    /// Whether `temps setup` has been run at least once. The web onboarding
    /// wizard checks this field on load and skips itself when true.
    pub setup_complete: bool,

    /// Cluster-DNS resolver settings (ADR-024, experimental beta). No masking
    /// needed — `enabled` is a plain bool with no sensitive content. Passed
    /// through as-is so the settings UI can read and toggle the flag.
    pub cluster_dns: ClusterDnsSettings,
}

/// Monitoring settings with the ClickHouse DSN masked.
///
/// `clickhouse_url` can embed credentials (`http://user:pass@host`), so it is
/// reported only as a boolean (`clickhouse_url_set`) rather than echoed back —
/// consistent with how the DNS API key and Docker registry password are masked.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MonitoringSettingsMasked {
    pub enabled: bool,
    pub store: MetricsStoreKind,
    pub scrape_interval_secs: u64,
    pub retention_raw_days: u32,
    pub retention_hourly_days: u32,
    pub retention_daily_years: u32,
    /// True when a ClickHouse DSN is configured. The DSN itself is never
    /// returned over HTTP because it may contain credentials.
    pub clickhouse_url_set: bool,
}

impl From<temps_core::MonitoringSettings> for MonitoringSettingsMasked {
    fn from(m: temps_core::MonitoringSettings) -> Self {
        Self {
            enabled: m.enabled,
            store: m.store,
            scrape_interval_secs: m.scrape_interval_secs,
            retention_raw_days: m.retention_raw_days,
            retention_hourly_days: m.retention_hourly_days,
            retention_daily_years: m.retention_daily_years,
            clickhouse_url_set: m
                .clickhouse_url
                .as_ref()
                .is_some_and(|u| !u.trim().is_empty()),
        }
    }
}

/// Agent sandbox settings with masked per-provider credentials.
/// Each provider entry reports only whether a credential is saved, not
/// the encrypted blob itself. Non-sensitive fields (auth_type, default_model,
/// extra) are passed through so the UI can render provider-specific state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentSandboxSettingsMasked {
    pub default_provider: String,
    pub providers: std::collections::HashMap<String, ProviderConfigMasked>,
    // Legacy top-level credential — reported only as a boolean
    pub api_key_saved: bool,
    pub auth_type: String,
    pub enabled: bool,
    pub runtime: String,
    pub custom_image: String,
    pub cpu_limit: f64,
    pub memory_limit_mb: u64,
    pub network_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderConfigMasked {
    pub auth_type: String,
    /// True if a credential is stored for this provider. The encrypted blob
    /// is never returned over HTTP.
    pub credential_saved: bool,
    pub default_model: Option<String>,
    pub extra: serde_json::Value,
}

/// Preview gateway settings with `shared_secret` elided.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PreviewGatewaySettingsMasked {
    pub image: String,
    pub host_port: u16,
    pub auto_upgrade: bool,
    pub shared_secret_set: bool,
}

/// Multi-node settings with `join_token_hash` elided.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MultiNodeSettingsMasked {
    pub has_join_token: bool,
    pub private_address: Option<String>,
    /// Whether control-plane↔agent mutual TLS is enforced.
    pub require_mtls: bool,
    /// Whether the deprecated shared join token is still accepted.
    pub legacy_shared_token_enabled: bool,
    /// SHA-256 fingerprint of the cluster CA certificate (public — operators can
    /// verify it out of band; the CA private key is never exposed).
    pub cluster_ca_fingerprint: Option<String>,
    /// Node resource-alert thresholds (percent); `None` = that alert disabled.
    pub node_cpu_alert_percent: Option<f64>,
    pub node_memory_alert_percent: Option<f64>,
    pub node_disk_alert_percent: Option<f64>,
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
            internal_url: settings.internal_url,
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
            agent_sandbox: AgentSandboxSettingsMasked {
                default_provider: settings.agent_sandbox.default_provider,
                providers: settings
                    .agent_sandbox
                    .providers
                    .into_iter()
                    .map(|(id, cfg)| {
                        (
                            id,
                            ProviderConfigMasked {
                                auth_type: cfg.auth_type,
                                credential_saved: cfg.credentials_encrypted.is_some(),
                                default_model: cfg.default_model,
                                extra: cfg.extra,
                            },
                        )
                    })
                    .collect(),
                api_key_saved: settings.agent_sandbox.api_key_encrypted.is_some(),
                auth_type: settings.agent_sandbox.auth_type,
                enabled: settings.agent_sandbox.enabled,
                runtime: settings.agent_sandbox.runtime,
                custom_image: settings.agent_sandbox.custom_image,
                cpu_limit: settings.agent_sandbox.cpu_limit,
                memory_limit_mb: settings.agent_sandbox.memory_limit_mb,
                network_mode: settings.agent_sandbox.network_mode,
            },
            ai_config: settings.ai_config,
            preview_gateway: PreviewGatewaySettingsMasked {
                image: settings.preview_gateway.image,
                host_port: settings.preview_gateway.host_port,
                auto_upgrade: settings.preview_gateway.auto_upgrade,
                shared_secret_set: !settings.preview_gateway.shared_secret.is_empty(),
            },
            multi_node: MultiNodeSettingsMasked {
                has_join_token: settings.multi_node.join_token_hash.is_some(),
                require_mtls: settings.multi_node.require_mtls,
                legacy_shared_token_enabled: settings.multi_node.legacy_shared_token_enabled,
                cluster_ca_fingerprint: settings
                    .multi_node
                    .cluster_ca_cert_pem
                    .as_deref()
                    .and_then(|pem| temps_core::node_pki::ca_fingerprint_sha256(pem).ok()),
                node_cpu_alert_percent: settings.multi_node.node_cpu_alert_percent,
                node_memory_alert_percent: settings.multi_node.node_memory_alert_percent,
                node_disk_alert_percent: settings.multi_node.node_disk_alert_percent,
                private_address: settings.multi_node.private_address,
            },
            // `effective_metrics_store` defaults to the configured store here;
            // the handler overrides it with the runtime-reconciled value once
            // the ClickHouse env-var state is known (via `with_effective_store`).
            effective_metrics_store: settings.monitoring.store.clone(),
            monitoring: MonitoringSettingsMasked::from(settings.monitoring),
            insecure_tls: settings.insecure_tls,
            setup_complete: settings.setup_complete,
            cluster_dns: settings.cluster_dns,
        }
    }
}

impl AppSettingsResponse {
    /// Reconcile `effective_metrics_store` with the server's ClickHouse
    /// configuration. The runtime only uses ClickHouse when both the
    /// `monitoring.store` toggle is `click_house` AND all `TEMPS_CLICKHOUSE_*`
    /// env vars are set (`clickhouse_enabled`); otherwise it falls back to
    /// TimescaleDB. This mirrors `build_ch_metrics_store` in the serve path so
    /// the UI reports the backend metrics actually land in.
    fn with_effective_store(mut self, clickhouse_enabled: bool) -> Self {
        self.effective_metrics_store =
            if self.monitoring.store == MetricsStoreKind::ClickHouse && clickhouse_enabled {
                MetricsStoreKind::ClickHouse
            } else {
                MetricsStoreKind::TimescaleDb
            };
        self
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_settings,
        get_disk_status,
        update_settings,
        generate_join_token,
        revoke_join_token,
        get_join_token_status,
        mint_enrollment_token,
        list_enrollment_tokens,
        revoke_enrollment_token,
        refresh_route_table,
    ),
    components(schemas(
        AppSettings,
        AppSettingsResponse,
        crate::disk_status::DiskInfo,
        crate::disk_status::DiskSpaceAlert,
        crate::disk_status::DiskSpaceCheckResult,
        ContainerLogSettings,
        ClusterDnsSettings,
        DnsProviderSettingsMasked,
        DockerRegistrySettingsMasked,
        AgentSandboxSettingsMasked,
        ProviderConfigMasked,
        PreviewGatewaySettingsMasked,
        MultiNodeSettingsMasked,
        MonitoringSettingsMasked,
        MetricsStoreKind,
        SettingsUpdateResponse,
        GenerateJoinTokenResponse,
        JoinTokenStatusResponse,
        MintEnrollmentTokenRequest,
        MintEnrollmentTokenResponse,
        EnrollmentTokenInfo,
        EnrollmentTokenListResponse,
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
        .route("/settings", get(get_settings))
        .route("/settings", put(update_settings))
        .route("/settings/disk-status", get(get_disk_status))
        .route("/settings/join-token/generate", post(generate_join_token))
        .route("/settings/join-token", delete(revoke_join_token))
        .route("/settings/join-token/status", get(get_join_token_status))
        .route(
            "/settings/enrollment-tokens",
            post(mint_enrollment_token).get(list_enrollment_tokens),
        )
        .route(
            "/settings/enrollment-tokens/{id}",
            delete(revoke_enrollment_token),
        )
        .route("/settings/routes/refresh", post(refresh_route_table))
}

// ── Node enrollment tokens (ADR-020 WS-1.1) ──────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct MintEnrollmentTokenRequest {
    /// Maximum registrations this token may authorize (default 1).
    pub max_uses: Option<i32>,
    /// Time-to-live in seconds (default 3600 = 1h).
    pub ttl_secs: Option<i64>,
    /// Optional: restrict the token to register one specific node name.
    pub bound_node_name: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MintEnrollmentTokenResponse {
    pub id: i32,
    /// The plaintext enrollment token — shown only once, save it now.
    pub token: String,
    pub expires_at: String,
    pub max_uses: i32,
    /// SHA-256 fingerprint of the cluster CA (if mTLS is set up). Pass it to the
    /// worker as `temps join --ca-fingerprint <fp>` to verify the CA on join.
    pub ca_fingerprint: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnrollmentTokenInfo {
    pub id: i32,
    pub expires_at: String,
    pub used_count: i32,
    pub max_uses: i32,
    pub bound_node_name: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnrollmentTokenListResponse {
    pub tokens: Vec<EnrollmentTokenInfo>,
}

fn enrollment_error_to_problem(e: crate::enrollment_tokens::EnrollmentError) -> Problem {
    use crate::enrollment_tokens::EnrollmentError;
    match e {
        EnrollmentError::Validation { message } => ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Validation Error")
            .detail(message)
            .build(),
        EnrollmentError::NotFound { id } => ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Enrollment Token Not Found")
            .detail(format!("Enrollment token {} not found", id))
            .build(),
        EnrollmentError::InvalidToken
        | EnrollmentError::Expired
        | EnrollmentError::Revoked
        | EnrollmentError::Exhausted => ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid Enrollment Token")
            .detail(e.to_string())
            .build(),
        EnrollmentError::Database(err) => {
            error!("Enrollment token DB error: {}", err);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Internal Server Error")
                .detail("Database error")
                .build()
        }
    }
}

/// Mint a short-lived, single-use node enrollment token.
#[utoipa::path(
    tag = "Settings",
    post,
    path = "/settings/enrollment-tokens",
    request_body = MintEnrollmentTokenRequest,
    responses(
        (status = 200, description = "Enrollment token minted", body = MintEnrollmentTokenResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn mint_enrollment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
    Json(req): Json<MintEnrollmentTokenRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    // If a cluster CA already exists, embed its SHA-256 fingerprint so a joining
    // node can verify the control plane's CA out of band (ADR-020 WS-2.2). The
    // CA is minted lazily on the first mTLS enrollment, so the very first token
    // may carry no fingerprint; subsequent tokens do.
    let settings = app_state.config_service.get_settings().await.map_err(|e| {
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Settings Error")
            .detail(format!("Failed to read settings: {e}"))
            .build()
    })?;
    let ca_fingerprint = settings
        .multi_node
        .cluster_ca_cert_pem
        .as_deref()
        .and_then(|pem| temps_core::node_pki::ca_fingerprint_sha256(pem).ok());

    let params = crate::enrollment_tokens::MintParams {
        max_uses: req.max_uses.unwrap_or(1),
        ttl_secs: req.ttl_secs.unwrap_or(3600),
        bound_node_name: req
            .bound_node_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        bound_labels: None,
        created_by_user_id: Some(auth.user_id()),
        ca_fingerprint: ca_fingerprint.clone(),
    };

    let (plaintext, model) = app_state
        .enrollment_token_service
        .mint(params)
        .await
        .map_err(enrollment_error_to_problem)?;

    info!(
        user_id = auth.user_id(),
        token_id = model.id,
        "Node enrollment token minted"
    );

    Ok(Json(MintEnrollmentTokenResponse {
        id: model.id,
        token: plaintext,
        expires_at: model.expires_at.to_rfc3339(),
        max_uses: model.max_uses,
        ca_fingerprint,
        message: "Enrollment token minted. Save it now — it will not be shown again.".to_string(),
    }))
}

/// List currently-valid node enrollment tokens (hashes elided).
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/enrollment-tokens",
    responses(
        (status = 200, description = "Active enrollment tokens", body = EnrollmentTokenListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn list_enrollment_tokens(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let tokens = app_state
        .enrollment_token_service
        .list_active()
        .await
        .map_err(enrollment_error_to_problem)?;

    let tokens = tokens
        .into_iter()
        .map(|t| EnrollmentTokenInfo {
            id: t.id,
            expires_at: t.expires_at.to_rfc3339(),
            used_count: t.used_count,
            max_uses: t.max_uses,
            bound_node_name: t.bound_node_name,
            created_at: t.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(EnrollmentTokenListResponse { tokens }))
}

/// Revoke a node enrollment token by id.
#[utoipa::path(
    tag = "Settings",
    delete,
    path = "/settings/enrollment-tokens/{id}",
    params(("id" = i32, Path, description = "Enrollment token id")),
    responses(
        (status = 200, description = "Enrollment token revoked", body = SettingsUpdateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Enrollment token not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn revoke_enrollment_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
    axum::extract::Path(id): axum::extract::Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    app_state
        .enrollment_token_service
        .revoke(id)
        .await
        .map_err(enrollment_error_to_problem)?;

    info!(
        user_id = auth.user_id(),
        token_id = id,
        "Node enrollment token revoked"
    );

    Ok(Json(SettingsUpdateResponse {
        message: format!("Enrollment token {} revoked", id),
    }))
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
            // Convert to response type that masks sensitive fields, then
            // reconcile the effective metrics store with the server's
            // ClickHouse env-var configuration so the UI shows the backend the
            // runtime actually uses (not just the DB toggle).
            let response = AppSettingsResponse::from(settings)
                .with_effective_store(app_state.config_service.is_clickhouse_enabled());
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

/// Get current disk usage for the control-plane server
///
/// Returns live disk usage for the monitored path along with any disks that
/// meet or exceed the configured alert threshold. Read-only — does not send
/// notifications. Used by the dashboard to surface a low-disk-space warning.
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/disk-status",
    responses(
        (status = 200, description = "Current disk usage and threshold alerts", body = DiskSpaceCheckResult),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_disk_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let status = crate::disk_status::collect_disk_status(&app_state.config_service)
        .await
        .map_err(|e| {
            tracing::error!("Failed to collect disk status: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .type_("https://temps.sh/probs/disk-status-error")
                .title("Disk Status Error")
                .detail(e.to_string())
                .build()
        })?;

    Ok(Json(status))
}

/// Restore settings fields that are recorded by the system itself and must not
/// be writable through the public `PUT /settings` API, copying them from the
/// current DB state onto the incoming payload.
///
/// Currently just `console_version` (ADR-017 Phase 3): a starting console
/// process records its binary version so a sibling `temps proxy` can warn on
/// version skew. The GET response never carries it, so without this an operator
/// round-trip would either spoof it or silently wipe it (`#[serde(default)]` →
/// `None`). Kept as a small pure helper so the invariant is unit-testable.
fn preserve_self_recorded_fields(incoming: &mut AppSettings, current: &AppSettings) {
    incoming.console_version = current.console_version.clone();
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

    // Merge sensitive sandbox/gateway/multi-node fields back from DB. The GET
    // endpoint strips encrypted credentials, shared secrets, and token hashes,
    // so any client round-trip would otherwise wipe them on save. We always
    // preserve them from the DB unless the incoming payload explicitly sets
    // them (e.g. a fresh credential save via the AI Providers page).
    match app_state.config_service.get_settings().await {
        Ok(current_settings) => {
            // `console_version` is self-recorded state, written only by a starting
            // console process (ADR-017 Phase 3 skew detection) and never exposed in
            // the GET response. Always restore it from the DB so an operator's
            // settings save can neither overwrite it (spoofing the skew check) nor
            // silently wipe it (a GET-then-PUT round-trip carries no value →
            // `#[serde(default)]` → None). Done first, before any field is moved
            // out of `current_settings` below.
            preserve_self_recorded_fields(&mut settings, &current_settings);

            // Per-provider credentials: keep existing unless caller supplied a new one
            for (id, current_cfg) in current_settings.agent_sandbox.providers.iter() {
                match settings.agent_sandbox.providers.get_mut(id) {
                    Some(incoming) => {
                        // Caller didn't include credentials -> restore from DB
                        if incoming
                            .credentials_encrypted
                            .as_deref()
                            .map(|s| s.is_empty() || s == "******")
                            .unwrap_or(true)
                        {
                            incoming.credentials_encrypted =
                                current_cfg.credentials_encrypted.clone();
                        }
                    }
                    None => {
                        // Caller dropped the provider entry entirely -> put it back
                        settings
                            .agent_sandbox
                            .providers
                            .insert(id.clone(), current_cfg.clone());
                    }
                }
            }
            // Legacy flat credential
            if settings
                .agent_sandbox
                .api_key_encrypted
                .as_deref()
                .map(|s| s.is_empty() || s == "******")
                .unwrap_or(true)
            {
                settings.agent_sandbox.api_key_encrypted =
                    current_settings.agent_sandbox.api_key_encrypted;
            }
            // Preview gateway shared secret
            if settings.preview_gateway.shared_secret.is_empty() {
                settings.preview_gateway.shared_secret =
                    current_settings.preview_gateway.shared_secret;
            }
            // Multi-node join token hash (never comes back from the mask response)
            if settings.multi_node.join_token_hash.is_none() {
                settings.multi_node.join_token_hash = current_settings.multi_node.join_token_hash;
            }
            // ClickHouse DSN: the GET response masks it to `clickhouse_url_set`
            // (it can embed credentials), so a client round-trip that doesn't
            // re-supply it would otherwise wipe the stored DSN — and then trip
            // the "clickhouse_url required when store is ClickHouse" validation
            // below on an unrelated save. Restore from the DB when absent.
            if settings
                .monitoring
                .clickhouse_url
                .as_deref()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
            {
                settings.monitoring.clickhouse_url = current_settings.monitoring.clickhouse_url;
            }
        }
        Err(e) => {
            tracing::warn!(
                "Could not fetch current settings to preserve sensitive fields: {}",
                e
            );
        }
    }

    // Validate monitoring settings fields.
    {
        let m = &settings.monitoring;
        if m.scrape_interval_secs < 15 {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .detail("monitoring.scrape_interval_secs must be >= 15")
                .build());
        }
        if m.retention_raw_days < 1 || m.retention_raw_days > 30 {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .detail("monitoring.retention_raw_days must be between 1 and 30")
                .build());
        }
        if m.retention_hourly_days < 7 || m.retention_hourly_days > 365 {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .detail("monitoring.retention_hourly_days must be between 7 and 365")
                .build());
        }
        if m.store == MetricsStoreKind::ClickHouse {
            match &m.clickhouse_url {
                None => {
                    return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                        .detail("monitoring.clickhouse_url is required when store is ClickHouse")
                        .build());
                }
                Some(url) if url::Url::parse(url).is_err() => {
                    return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                        .detail("monitoring.clickhouse_url is not a valid URL")
                        .build());
                }
                _ => {}
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

    // Validate and sanitize internal_url (same rules as external_url)
    if let Some(ref mut int_url) = settings.internal_url {
        *int_url = int_url.trim().trim_end_matches('/').to_string();
        if int_url.is_empty() {
            settings.internal_url = None;
        } else {
            if !int_url.starts_with("http://") && !int_url.starts_with("https://") {
                return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .detail("Internal URL must start with http:// or https://")
                    .build());
            }
            if int_url.contains('#') || int_url.contains('?') {
                return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .detail("Internal URL must not contain '#' or '?' characters")
                    .build());
            }
            if url::Url::parse(int_url).is_err() {
                return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .detail("Internal URL is not a valid URL")
                    .build());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::{AgentSandboxSettings, AppSettings, ProviderConfig};

    // Regression: the GET /api/settings response must surface agent_sandbox,
    // ai_config, preview_gateway, multi_node, and insecure_tls so the UI can
    // render (and round-trip) resource/runtime/network settings. An earlier
    // version silently dropped them, making every save from the Sandbox page
    // appear not to persist.
    #[test]
    fn response_surfaces_all_sandbox_related_settings() {
        let settings = AppSettings {
            agent_sandbox: AgentSandboxSettings {
                default_provider: "claude_cli".into(),
                providers: [(
                    "claude_cli".to_string(),
                    ProviderConfig {
                        auth_type: "api_key".into(),
                        credentials_encrypted: Some("super-secret-blob".into()),
                        default_model: Some("sonnet".into()),
                        extra: serde_json::Value::Null,
                    },
                )]
                .into_iter()
                .collect(),
                auth_type: "api_key".into(),
                api_key_encrypted: Some("legacy-secret".into()),
                enabled: true,
                runtime: "python".into(),
                custom_image: String::new(),
                cpu_limit: 8.0,
                memory_limit_mb: 16_384,
                network_mode: "restricted".into(),
            },
            ..Default::default()
        };

        let response = AppSettingsResponse::from(settings);

        assert_eq!(response.agent_sandbox.cpu_limit, 8.0);
        assert_eq!(response.agent_sandbox.memory_limit_mb, 16_384);
        assert_eq!(response.agent_sandbox.runtime, "python");
        assert_eq!(response.agent_sandbox.network_mode, "restricted");
        assert!(response.agent_sandbox.enabled);
        let provider = response
            .agent_sandbox
            .providers
            .get("claude_cli")
            .expect("provider entry should round-trip");
        assert!(
            provider.credential_saved,
            "credential presence must survive"
        );
        assert_eq!(provider.default_model.as_deref(), Some("sonnet"));
        assert!(response.agent_sandbox.api_key_saved);
    }

    // Sensitive blobs must never leak through the response type, even though
    // they're encrypted at rest. The UI asks for booleans, not the real ciphertext.
    #[test]
    fn response_never_exposes_encrypted_credentials() {
        let mut settings = AppSettings::default();
        settings.agent_sandbox.providers.insert(
            "claude_cli".into(),
            ProviderConfig {
                auth_type: "api_key".into(),
                credentials_encrypted: Some("super-secret-blob".into()),
                default_model: None,
                extra: serde_json::Value::Null,
            },
        );
        settings.agent_sandbox.api_key_encrypted = Some("legacy-secret".into());
        settings.preview_gateway.shared_secret = "preview-token".into();
        settings.multi_node.join_token_hash = Some("hash".into());

        let response = AppSettingsResponse::from(settings);
        let json = serde_json::to_string(&response).expect("serialize response");

        assert!(!json.contains("super-secret-blob"));
        assert!(!json.contains("legacy-secret"));
        assert!(!json.contains("preview-token"));
        assert!(!json.contains("\"hash\""));
        assert!(json.contains("\"credential_saved\":true"));
        assert!(json.contains("\"shared_secret_set\":true"));
        assert!(json.contains("\"has_join_token\":true"));
    }

    // Regression: the GET /api/settings response must surface `monitoring` so
    // the Metrics Monitoring page reflects persisted settings instead of
    // silently falling back to client-side defaults. The ClickHouse DSN must
    // be masked (it can embed credentials).
    #[test]
    fn response_surfaces_monitoring_with_masked_dsn() {
        let mut settings = AppSettings::default();
        settings.monitoring.enabled = true;
        settings.monitoring.store = MetricsStoreKind::ClickHouse;
        settings.monitoring.scrape_interval_secs = 60;
        settings.monitoring.retention_raw_days = 14;
        settings.monitoring.clickhouse_url = Some("http://ch-user:ch-pass@clickhouse:8123".into());

        let response = AppSettingsResponse::from(settings);

        assert!(response.monitoring.enabled);
        assert_eq!(response.monitoring.store, MetricsStoreKind::ClickHouse);
        assert_eq!(response.monitoring.scrape_interval_secs, 60);
        assert_eq!(response.monitoring.retention_raw_days, 14);
        assert!(response.monitoring.clickhouse_url_set);

        // The DSN (and its embedded credentials) must never serialize.
        let json = serde_json::to_string(&response).expect("serialize response");
        assert!(!json.contains("ch-pass"));
        assert!(!json.contains("clickhouse:8123"));
        assert!(json.contains("\"clickhouse_url_set\":true"));
    }

    // The effective metrics store reconciles the `store` toggle with the
    // server's ClickHouse env-var state, mirroring `build_ch_metrics_store`.
    #[test]
    fn effective_store_reflects_runtime_clickhouse_availability() {
        // store=click_house but env vars NOT configured → runtime uses Timescale.
        let mut settings = AppSettings::default();
        settings.monitoring.store = MetricsStoreKind::ClickHouse;
        let response = AppSettingsResponse::from(settings.clone()).with_effective_store(false);
        assert_eq!(response.monitoring.store, MetricsStoreKind::ClickHouse);
        assert_eq!(
            response.effective_metrics_store,
            MetricsStoreKind::TimescaleDb,
            "ClickHouse selected but env vars unset must fall back to TimescaleDB"
        );

        // store=click_house AND env vars configured → runtime uses ClickHouse.
        let response = AppSettingsResponse::from(settings).with_effective_store(true);
        assert_eq!(
            response.effective_metrics_store,
            MetricsStoreKind::ClickHouse
        );

        // store=timescale_db → always TimescaleDB, regardless of env vars.
        let response = AppSettingsResponse::from(AppSettings::default()).with_effective_store(true);
        assert_eq!(
            response.effective_metrics_store,
            MetricsStoreKind::TimescaleDb
        );
    }

    // ADR-017 Phase 3: `console_version` is self-recorded by a starting console
    // and must never be writable via the public PUT /settings API. The GET
    // response strips it, so a normal UI round-trip sends no value — without the
    // preserve step that would wipe the stored version (degrading skew
    // detection), and a crafted body could spoof it.
    #[test]
    fn update_preserves_console_version_when_payload_omits_it() {
        // Simulates the common UI round-trip: GET (no console_version) then PUT.
        let mut incoming = AppSettings::default();
        assert_eq!(incoming.console_version, None);

        let current = AppSettings {
            console_version: Some("v0.1.0".into()),
            ..Default::default()
        };

        preserve_self_recorded_fields(&mut incoming, &current);
        assert_eq!(
            incoming.console_version.as_deref(),
            Some("v0.1.0"),
            "an omitted console_version must be restored from the DB, not wiped"
        );
    }

    #[test]
    fn update_rejects_attempt_to_overwrite_console_version() {
        // An operator (or crafted client) tries to spoof the recorded version.
        let mut incoming = AppSettings {
            console_version: Some("v9.9.9-spoofed".into()),
            ..Default::default()
        };
        let current = AppSettings {
            console_version: Some("v0.1.0".into()),
            ..Default::default()
        };

        preserve_self_recorded_fields(&mut incoming, &current);
        assert_eq!(
            incoming.console_version.as_deref(),
            Some("v0.1.0"),
            "the API must not be able to overwrite the self-recorded console_version"
        );
    }

    // ADR-024: cluster_dns must be visible in the GET /settings response so
    // operators can read and toggle the feature flag. No masking — it's a
    // plain bool with no sensitive content.
    #[test]
    fn response_surfaces_cluster_dns_disabled_by_default() {
        let settings = AppSettings::default();
        let response = AppSettingsResponse::from(settings);
        assert!(
            !response.cluster_dns.enabled,
            "cluster_dns.enabled must be false in the default response"
        );
    }

    #[test]
    fn response_surfaces_cluster_dns_when_enabled() {
        let mut settings = AppSettings::default();
        settings.cluster_dns.enabled = true;
        let response = AppSettingsResponse::from(settings);
        assert!(
            response.cluster_dns.enabled,
            "cluster_dns.enabled=true must survive the AppSettings->AppSettingsResponse conversion"
        );
        // Confirm it serializes into the JSON response body
        let json = serde_json::to_string(&response).expect("serialize response");
        assert!(
            json.contains("\"cluster_dns\""),
            "cluster_dns must appear in the settings response JSON"
        );
        assert!(json.contains("\"enabled\":true"));
    }

    // The precondition that makes the preserve step necessary: the GET response
    // never carries console_version, so the field cannot round-trip from a
    // client and would default to None on any PUT.
    #[test]
    fn response_never_exposes_console_version() {
        let settings = AppSettings {
            console_version: Some("v0.1.0".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&AppSettingsResponse::from(settings))
            .expect("serialize response");
        assert!(
            !json.contains("console_version"),
            "console_version must not appear in the settings response"
        );
        assert!(!json.contains("v0.1.0"));
    }
}
