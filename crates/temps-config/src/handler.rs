use crate::disk_status::DiskSpaceCheckResult;
use crate::{ConfigService, EffectiveTelemetryPolicies};
use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::{
    problemdetails::Problem, AiChatLimitsSettings, AiConfigSettings, AppSettings, AuditContext,
    AuditLogger, AuditOperation, BuildLimitsSettings, ClusterDnsSettings, ContainerLogSettings,
    DiskSpaceAlertSettings, LetsEncryptSettings, MetricsStoreKind, MonitoringSettings,
    ObservabilityCompressionSettings, ObservabilityRetentionSettings, PublicHostnameStrategy,
    RateLimitSettings, RequestMetadata, ScreenshotSettings, SecurityHeadersSettings,
};
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

pub struct SettingsState {
    pub config_service: Arc<ConfigService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub route_table_refresher: Option<Arc<dyn temps_core::route_table::RouteTableRefresher>>,
    /// Node enrollment token minting/listing/revocation (ADR-020 WS-1.1).
    pub enrollment_token_service: Arc<crate::enrollment_tokens::EnrollmentTokenService>,
    /// Result slot of the background release-update notifier (`temps serve`
    /// writes it). `None` in host processes that don't run the notifier
    /// (e.g. the standalone proxy's plugin context) — the update-status
    /// endpoint then reports "no update known".
    pub update_status: Option<Arc<temps_core::UpdateStatusSlot>>,
    /// Applies a release and restarts the server. `None` in hosts that cannot
    /// meaningfully restart themselves (e.g. the standalone proxy) — the
    /// update endpoints then report the feature as unsupported here rather
    /// than pretending it is merely misconfigured.
    pub self_updater: Option<Arc<dyn temps_core::SelfUpdater>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SettingsUpdatedAudit {
    context: AuditContext,
}

impl AuditOperation for SettingsUpdatedAudit {
    fn operation_type(&self) -> String {
        "SETTINGS_UPDATED".to_string()
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

/// Audit record for a console-triggered platform update. Written before the
/// process exits, so the trail survives the restart it causes.
#[derive(Debug, Clone, serde::Serialize)]
struct PlatformUpdateStartedAudit {
    context: AuditContext,
    /// Version the server was running when the update was requested.
    from_version: String,
    /// Explicitly pinned target, or `None` for "newest on this channel".
    target_version: Option<String>,
}

impl AuditOperation for PlatformUpdateStartedAudit {
    fn operation_type(&self) -> String {
        "PLATFORM_UPDATE_STARTED".to_string()
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
    /// Public edge target that synced DNS records point at (IP → A/AAAA, else CNAME).
    pub edge_target: Option<String>,
    /// Port the main Pingora proxy listens on (parsed from `--address`), the
    /// same value `ConfigService::proxy_port()` feeds into
    /// `compute_deployment_url`/`compute_environment_url` when `external_url`
    /// is unset. The console uses this to preview a project's real
    /// `{slug}-{env_slug}.{preview_domain}:{port}` URL before it's deployed.
    pub proxy_port: u16,

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

    /// Number of enabled, running services the MetricsScraper currently
    /// includes. Used for the lightweight storage estimate in the UI.
    pub monitored_services_count: Option<u64>,

    /// TimescaleDB compression delays for immutable proxy logs and OTel spans.
    pub observability_compression: ObservabilityCompressionSettings,

    /// Retention windows for raw proxy logs and OpenTelemetry data.
    pub observability_retention: ObservabilityRetentionSettings,

    /// The storage backend the runtime is **actually** using for metrics,
    /// after reconciling the `monitoring.store` toggle with the server's
    /// `TEMPS_CLICKHOUSE_*` configuration. When `monitoring.store` is
    /// `click_house` but those env vars are not fully set, the runtime falls
    /// back to TimescaleDB — in that case this reports `timescale_db` even
    /// though `monitoring.store` says `click_house`. The UI shows this as the
    /// effective backend and warns when it diverges from the configured store.
    pub effective_metrics_store: MetricsStoreKind,

    /// Storage backend actually used for proxy logs, OTel spans, and OTel
    /// metrics. OTel logs remain TimescaleDB-backed. Unlike resource metrics,
    /// these domains switch to ClickHouse whenever the server-level ClickHouse
    /// connection is configured; they do not use the monitoring store toggle.
    pub effective_observability_store: MetricsStoreKind,

    // Outbound TLS verification toggle
    pub insecure_tls: bool,

    /// Whether `temps setup` has been run at least once. The web onboarding
    /// wizard checks this field on load and skips itself when true.
    pub setup_complete: bool,

    /// When enabled, Admin-role accounts without MFA enrolled are rejected
    /// at password login (bherila/temps#32). SSO/OIDC logins are unaffected.
    pub require_mfa_for_admins: bool,

    /// Cluster-DNS resolver settings (ADR-024, experimental beta). No masking
    /// needed — `enabled` is a plain bool with no sensitive content. Passed
    /// through as-is so the settings UI can read and toggle the flag.
    pub cluster_dns: ClusterDnsSettings,

    /// Build-time resource limits (control-plane only). No sensitive content,
    /// passed through as-is.
    pub build_limits: BuildLimitsSettings,

    /// Per-turn limits for the AI chat. No sensitive content.
    pub ai_chat_limits: AiChatLimitsSettings,
    /// Whether admins may apply a release from the console. This is the
    /// database-backed toggle only — a server started with
    /// `--disable-self-update` refuses regardless of what this says, which
    /// `GET /settings/update` reports as the authoritative answer.
    pub self_update: temps_core::SelfUpdateSettings,
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
    pub sandbox_backend: String,
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
        // Resolved before the literal below starts moving fields out of
        // `settings`; absence means "never configured", which reads as default.
        let self_update = settings.self_update();
        Self {
            external_url: settings.external_url,
            internal_url: settings.internal_url,
            preview_domain: settings.preview_domain,
            edge_target: settings.edge_target,
            // Overridden by the handler via `with_proxy_port` — this struct
            // has no access to `ConfigService` here, only the DB-backed
            // `AppSettings` row. 8080 mirrors `ConfigService::proxy_port()`'s
            // own fallback so an un-reconciled response is never worse than
            // that.
            proxy_port: 8080,
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
                sandbox_backend: settings
                    .agent_sandbox
                    .sandbox_backend
                    .unwrap_or_else(|| "docker".to_string()),
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
            effective_observability_store: MetricsStoreKind::TimescaleDb,
            monitoring: MonitoringSettingsMasked::from(settings.monitoring),
            monitored_services_count: None,
            observability_compression: settings.observability_compression,
            observability_retention: settings.observability_retention,
            insecure_tls: settings.insecure_tls,
            setup_complete: settings.setup_complete,
            require_mfa_for_admins: settings.require_mfa_for_admins,
            cluster_dns: settings.cluster_dns,
            build_limits: settings.build_limits,
            ai_chat_limits: settings.ai_chat_limits,
            self_update,
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
        self.effective_observability_store = if clickhouse_enabled {
            MetricsStoreKind::ClickHouse
        } else {
            MetricsStoreKind::TimescaleDb
        };
        self
    }

    /// Sets the real proxy listener port, resolved from `ConfigService`
    /// (unavailable to the plain `From<AppSettings>` conversion above).
    fn with_proxy_port(mut self, proxy_port: u16) -> Self {
        self.proxy_port = proxy_port;
        self
    }

    fn with_effective_timescale_state(
        mut self,
        policies: EffectiveTelemetryPolicies,
        monitored_services_count: Option<u64>,
    ) -> Self {
        self.monitored_services_count = monitored_services_count;

        if self.effective_metrics_store == MetricsStoreKind::TimescaleDb {
            if let Some(days) = policies.metrics_raw_days {
                self.monitoring.retention_raw_days = days;
            }
            if let Some(days) = policies.metrics_hourly_days {
                self.monitoring.retention_hourly_days = days;
            }
            if let Some(years) = policies.metrics_daily_years {
                self.monitoring.retention_daily_years = years;
            }
        }

        if self.effective_observability_store == MetricsStoreKind::TimescaleDb {
            if let Some(hours) = policies.proxy_logs_compression_hours {
                self.observability_compression.proxy_logs_after_hours = hours;
            }
            if let Some(hours) = policies.otel_spans_compression_hours {
                self.observability_compression.otel_spans_after_hours = hours;
            }
            if let Some(days) = policies.proxy_logs_retention_days {
                self.observability_retention.proxy_logs_days = days;
            }
            if let Some(days) = policies.otel_spans_retention_days {
                self.observability_retention.otel_spans_days = days;
            }
        }

        if let Some(days) = policies.otel_logs_retention_days {
            self.observability_retention.otel_logs_days = days;
        }
        if self.effective_observability_store == MetricsStoreKind::TimescaleDb {
            if let Some(days) = policies.otel_metrics_retention_days {
                self.observability_retention.otel_metrics_days = days;
            }
        }

        self
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_settings,
        get_update_status,
        get_update_capability,
        start_update,
        check_for_update,
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
        PublicHostnameStrategy,
        DnsProviderSettingsMasked,
        DockerRegistrySettingsMasked,
        AgentSandboxSettingsMasked,
        ProviderConfigMasked,
        PreviewGatewaySettingsMasked,
        MultiNodeSettingsMasked,
        MonitoringSettingsMasked,
        ObservabilityCompressionSettings,
        ObservabilityRetentionSettings,
        MetricsStoreKind,
        SettingsUpdateResponse,
        GenerateJoinTokenResponse,
        JoinTokenStatusResponse,
        MintEnrollmentTokenRequest,
        MintEnrollmentTokenResponse,
        EnrollmentTokenInfo,
        EnrollmentTokenListResponse,
        RouteRefreshResponse,
        UpdateStatusResponse,
        UpdateCapabilityResponse,
        StartUpdateRequest,
        StartUpdateResponse,
        temps_core::SelfUpdateSettings,
        temps_core::SelfUpdateAttempt,
        temps_core::SelfUpdateBlocker,
        temps_core::SelfUpdatePhase,
        temps_core::SelfUpdateRestartMode,
        temps_core::SelfUpdateStatus,
        temps_core::ReleaseCheckResult,
        temps_core::SupervisorKind,
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
        .route("/settings/update-status", get(get_update_status))
        .route(
            "/settings/update",
            get(get_update_capability).post(start_update),
        )
        .route("/settings/update/check", post(check_for_update))
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

/// Result of the background release-update check, driving the web console's
/// upgrade banner. All optional fields are set together iff
/// `update_available` is true.
#[derive(Debug, Serialize, ToSchema)]
pub struct UpdateStatusResponse {
    /// True when a newer release than the running binary has been published
    /// on this install's channel.
    pub update_available: bool,
    /// Version tag of the running binary, e.g. `v0.1.0-beta.45`.
    pub current_version: Option<String>,
    /// Newest published tag on this install's channel.
    pub latest_version: Option<String>,
    /// Channel the install tracks: `stable` or `beta`.
    pub channel: Option<String>,
    /// Release-notes page (GitHub release) for the newer version.
    pub release_url: Option<String>,
    /// When the check that found the update ran (ISO 8601, UTC).
    pub checked_at: Option<String>,
    /// Docs page with upgrade instructions. Always present so the UI links
    /// the same page regardless of update state.
    pub docs_url: String,
}

/// Report whether a newer temps release is available for this install.
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/update-status",
    responses(
        (status = 200, description = "Release update status for this install", body = UpdateStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
async fn get_update_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let update = app_state.update_status.as_ref().and_then(|slot| slot.get());
    let response = match update {
        Some(update) => UpdateStatusResponse {
            update_available: true,
            current_version: Some(update.current_version),
            latest_version: Some(update.latest_version),
            channel: Some(update.channel),
            release_url: Some(update.release_url),
            checked_at: Some(
                update
                    .checked_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
            docs_url: temps_core::UPGRADE_DOCS_URL.to_string(),
        },
        // Covers both "up to date" and "no check has succeeded yet" — the
        // banner is advisory, so the UI treats them identically.
        None => UpdateStatusResponse {
            update_available: false,
            current_version: None,
            latest_version: None,
            channel: None,
            release_url: None,
            checked_at: None,
            docs_url: temps_core::UPGRADE_DOCS_URL.to_string(),
        },
    };

    Ok(Json(response))
}

// ── Applying a release from the console ──────────────────────────────────────

/// Whether this install can apply a release update on request, and how the last
/// attempt went.
///
/// Deliberately answerable even when the answer is "no": an operator who cannot
/// use the button still needs to know *why* and what to run instead, so this
/// never 404s or returns an empty body when the feature is unavailable.
#[derive(Debug, Serialize, ToSchema)]
pub struct UpdateCapabilityResponse {
    /// True only when a request would actually download, install and restart.
    pub can_apply: bool,
    /// Whether the *caller* holds `platform:update`. Distinct from `can_apply`,
    /// which describes the server: the console shows the action only when both
    /// are true, so a reader is never offered a button that would 403.
    pub allowed: bool,
    /// Machine-readable reason `can_apply` is false (`disabled_by_flag`,
    /// `disabled_by_setting`, `container`, `no_supervisor`, `binary_not_writable`,
    /// `unsupported_platform`, `in_progress`).
    pub blocker: Option<temps_core::SelfUpdateBlocker>,
    /// Operator-facing explanation of `blocker`.
    pub reason: Option<String>,
    /// Non-blocking warning to show with the confirmation (split topology).
    pub caveat: Option<String>,
    /// The equivalent command to run by hand. Always present.
    pub manual_command: String,
    /// Version tag of the running binary. Always present — the version page
    /// needs it whether or not an update exists.
    pub current_version: String,
    /// Channel actually tracked, after applying the configured override or
    /// falling back to inference from the running version tag.
    pub channel: String,
    /// True when `channel` was set explicitly in settings rather than inferred.
    pub channel_is_pinned: bool,
    /// What would restart the process: `systemd`, `launchd`, `container`, `none`.
    pub supervisor: temps_core::SupervisorKind,
    /// `automatic` when applying an update also restarts temps; `manual` when
    /// it only installs the binary and the operator restarts on their own
    /// schedule. Lets the console set expectations before the click.
    pub restart_mode: temps_core::SelfUpdateRestartMode,
    /// Binary that would be replaced.
    pub binary_path: String,
    /// Phase of an in-flight attempt: `idle` when none is running.
    pub phase: temps_core::SelfUpdatePhase,
    /// Failure detail while `phase` is `failed`.
    pub phase_error: Option<String>,
    /// Most recent attempt, including one resolved during this boot — this is
    /// how the console reports the outcome of an update that restarted it.
    pub last_attempt: Option<temps_core::SelfUpdateAttempt>,
    /// Number of migrations applied so far. `Some` while `phase` is `migrating`.
    pub migrations_applied: Option<u32>,
    /// Total migrations to be applied. `Some` once the migrate child has
    /// reported its first `started` event.
    pub migrations_total: Option<u32>,
    /// Name of the migration currently running. `Some` while `phase` is
    /// `migrating` and a migration step is in flight.
    pub current_migration_name: Option<String>,
}

/// Optional pin for the version to install.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct StartUpdateRequest {
    /// Release tag to install (e.g. `v0.2.0`). Omit to take the newest release
    /// on the channel this install already tracks.
    pub version: Option<String>,
}

/// Acknowledgement that an update was accepted and is running.
#[derive(Debug, Serialize, ToSchema)]
pub struct StartUpdateResponse {
    /// Version the server is running as it accepts this request.
    pub current_version: String,
    /// How long to allow for the server to come back before treating the
    /// restart as failed. `0` when nothing restarts.
    pub estimated_restart_secs: u64,
    /// `automatic` (temps restarts itself) or `manual` (installed only).
    pub restart_mode: temps_core::SelfUpdateRestartMode,
    pub message: String,
}

/// Read the database-backed half of the update policy.
///
/// Fails CLOSED: if settings cannot be read we must not report (or act on) a
/// capability the operator may have deliberately turned off.
async fn load_self_update_policy(app_state: &SettingsState) -> temps_core::SelfUpdatePolicy {
    match app_state.config_service.get_settings().await {
        Ok(settings) => {
            let self_update = settings.self_update();
            temps_core::SelfUpdatePolicy {
                enabled: self_update.enabled,
                channel: self_update.channel,
            }
        }
        Err(e) => {
            error!("Could not read self-update settings, treating as disabled: {e}");
            temps_core::SelfUpdatePolicy {
                enabled: false,
                channel: None,
            }
        }
    }
}

/// Build the "no updater registered in this process" answer.
///
/// Reached in hosts that run the settings API without owning the process
/// lifecycle. Reported as a capability with a reason rather than an error, so
/// the console renders the same explain-and-point-at-the-CLI surface it uses
/// for every other blocked state.
fn updater_unavailable_response(allowed: bool) -> UpdateCapabilityResponse {
    UpdateCapabilityResponse {
        can_apply: false,
        allowed,
        blocker: Some(temps_core::SelfUpdateBlocker::NotSupported),
        reason: Some(
            "This process does not manage the temps binary, so it cannot apply an update. \
             Upgrade from the command line on the host instead."
                .to_string(),
        ),
        caveat: None,
        manual_command: "temps upgrade".to_string(),
        current_version: String::new(),
        channel: "unknown".to_string(),
        channel_is_pinned: false,
        supervisor: temps_core::SupervisorKind::None,
        restart_mode: temps_core::SelfUpdateRestartMode::Manual,
        binary_path: String::new(),
        phase: temps_core::SelfUpdatePhase::Idle,
        phase_error: None,
        last_attempt: None,
        migrations_applied: None,
        migrations_total: None,
        current_migration_name: None,
    }
}

/// Ask the release API for the newest version on this install's channel, now,
/// instead of waiting for the background notifier's next pass.
#[utoipa::path(
    tag = "Settings",
    post,
    path = "/settings/update/check",
    responses(
        (status = 200, description = "Result of the release check", body = temps_core::ReleaseCheckResult),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 502, description = "The release API could not be reached", body = temps_core::ProblemDetails),
        (status = 501, description = "This process cannot check for updates", body = temps_core::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn check_for_update(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    // A read-only network probe that changes no state the operator can't
    // already see, so it sits with the rest of the settings reads.
    permission_guard!(auth, SettingsRead);

    let Some(updater) = app_state.self_updater.as_ref() else {
        return Err(ErrorBuilder::new(StatusCode::NOT_IMPLEMENTED)
            .title("Update Checks Not Supported Here")
            .detail("This process does not track temps releases.")
            .build());
    };

    let policy = load_self_update_policy(&app_state).await;
    let result = updater.check_now(policy.channel).await.map_err(|reason| {
        // Upstream reachability, not a client mistake — say so plainly so the
        // operator looks at egress rather than at their own request.
        ErrorBuilder::new(StatusCode::BAD_GATEWAY)
            .title("Release Check Failed")
            .detail(reason)
            .build()
    })?;

    Ok(Json(result))
}

/// Report whether a release update can be applied from the console.
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/update",
    responses(
        (status = 200, description = "Self-update capability for this install", body = UpdateCapabilityResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
async fn get_update_capability(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    // Readable by anyone who can read settings: the banner needs this to decide
    // what to render. Actually *starting* an update needs `platform:update`,
    // reported separately as `allowed`.
    permission_guard!(auth, SettingsRead);

    let allowed = auth.has_permission(&temps_auth::Permission::PlatformUpdate);

    let Some(updater) = app_state.self_updater.as_ref() else {
        return Ok(Json(updater_unavailable_response(allowed)));
    };

    let capability = updater.capability(&load_self_update_policy(&app_state).await);
    Ok(Json(UpdateCapabilityResponse {
        // Describes the SERVER only. Permission is reported separately as
        // `allowed` so a blocked install and an under-privileged caller stay
        // distinguishable — collapsing them would leave the UI unable to say
        // which of the two it is looking at.
        can_apply: capability.can_apply,
        allowed,
        blocker: capability.blocker,
        reason: capability.reason,
        caveat: capability.caveat,
        manual_command: capability.manual_command,
        current_version: capability.current_version,
        channel: capability.channel,
        channel_is_pinned: capability.channel_is_pinned,
        supervisor: capability.supervisor,
        restart_mode: capability.restart_mode,
        // Host filesystem layout is only useful to someone who can actually
        // run an update; readers with `settings:read` alone get nothing from
        // it but a hint about where the install lives.
        binary_path: if allowed {
            capability.binary_path
        } else {
            String::new()
        },
        phase: capability.phase,
        phase_error: capability.phase_error,
        last_attempt: capability.last_attempt,
        migrations_applied: capability.migrations_applied,
        migrations_total: capability.migrations_total,
        current_migration_name: capability.current_migration_name,
    }))
}

/// Install a release and restart the server.
///
/// Returns as soon as the attempt is accepted: the download and swap run in the
/// background and the process then exits so its supervisor restarts it on the
/// new binary. Poll `GET /settings/update` for progress — after the restart,
/// `last_attempt` carries the outcome.
#[utoipa::path(
    tag = "Settings",
    post,
    path = "/settings/update",
    request_body = StartUpdateRequest,
    responses(
        (status = 202, description = "Update accepted; the server will restart", body = StartUpdateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Update unavailable or already running", body = temps_core::ProblemDetails),
        (status = 501, description = "This process cannot apply updates", body = temps_core::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn start_update(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<StartUpdateRequest>,
) -> Result<impl IntoResponse, Problem> {
    // NOT SettingsWrite: replacing the running binary and dropping every
    // in-flight request is a different class of action from editing a config
    // value, so it carries its own permission.
    permission_guard!(auth, PlatformUpdate);

    let Some(updater) = app_state.self_updater.as_ref() else {
        return Err(ErrorBuilder::new(StatusCode::NOT_IMPLEMENTED)
            .title("Self-Update Not Supported Here")
            .detail(
                "This process does not manage the temps binary. Upgrade from the command line \
                 on the host with `temps upgrade`.",
            )
            .build());
    };

    let started = updater
        .start(
            request.version.clone(),
            Some(auth.user_id()),
            &load_self_update_policy(&app_state).await,
        )
        .map_err(self_update_error_to_problem)?;

    // Audited BEFORE the restart — the process is about to exit, and an update
    // that leaves no trace of who triggered it is exactly the record an
    // operator needs afterwards.
    let audit = PlatformUpdateStartedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        from_version: started.current_version.clone(),
        target_version: request.version.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for platform update: {}", e);
    }

    info!(
        user_id = auth.user_id(),
        from = %started.current_version,
        target = ?request.version,
        "Platform update started from the console"
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(StartUpdateResponse {
            current_version: started.current_version,
            estimated_restart_secs: started.estimated_restart_secs,
            restart_mode: started.restart_mode,
            message: match started.restart_mode {
                temps_core::SelfUpdateRestartMode::Automatic => {
                    "Update started. The server will restart when the new binary is installed."
                }
                temps_core::SelfUpdateRestartMode::Manual => {
                    "Update started. The new binary will be installed, but temps keeps running \
                     the current version until you restart it."
                }
            }
            .to_string(),
        }),
    ))
}

fn self_update_error_to_problem(error: temps_core::SelfUpdateError) -> Problem {
    use temps_core::{SelfUpdateBlocker, SelfUpdateError};
    let status = match error {
        // These describe current state the caller can change (a flag, a
        // setting, a running attempt) rather than a malformed request.
        SelfUpdateError::Unavailable { .. } | SelfUpdateError::AlreadyRunning { .. } => {
            StatusCode::CONFLICT
        }
        // A bad argument, not a state of the install.
        SelfUpdateError::InvalidVersion { .. } => StatusCode::BAD_REQUEST,
    };
    let Some(blocker) = error.blocker() else {
        return ErrorBuilder::new(status)
            .title("Invalid Version")
            .detail(error.to_string())
            .build();
    };
    let title = match blocker {
        SelfUpdateBlocker::DisabledByFlag | SelfUpdateBlocker::DisabledBySetting => {
            "Self-Update Disabled"
        }
        SelfUpdateBlocker::InProgress => "Update Already Running",
        SelfUpdateBlocker::NotSupported => "Self-Update Not Supported Here",
        SelfUpdateBlocker::BinaryNotWritable => "Binary Not Writable",
        SelfUpdateBlocker::UnsupportedPlatform => "Unsupported Platform",
    };
    ErrorBuilder::new(status)
        .title(title)
        .detail(error.to_string())
        .value(
            "blocker",
            serde_json::to_value(blocker)
                .unwrap_or(serde_json::Value::Null)
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
        .build()
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

    let (settings_result, policies_result, monitored_services_result) = tokio::join!(
        app_state.config_service.get_settings(),
        app_state.config_service.get_effective_telemetry_policies(),
        app_state.config_service.count_monitored_services(),
    );

    match settings_result {
        Ok(settings) => {
            // Convert to response type that masks sensitive fields, then
            // reconcile the effective metrics store with the server's
            // ClickHouse env-var configuration so the UI shows the backend the
            // runtime actually uses (not just the DB toggle).
            let policies = policies_result.unwrap_or_else(|error| {
                tracing::warn!(
                    %error,
                    "Failed to read effective TimescaleDB policies; using configured values"
                );
                EffectiveTelemetryPolicies::default()
            });
            let monitored_services_count = monitored_services_result
                .inspect_err(|error| {
                    tracing::warn!(
                        %error,
                        "Failed to count monitored services; storage estimate is unavailable"
                    );
                })
                .ok();
            let response = AppSettingsResponse::from(settings)
                .with_effective_store(app_state.config_service.is_clickhouse_enabled())
                .with_effective_timescale_state(policies, monitored_services_count)
                .with_proxy_port(app_state.config_service.proxy_port());
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

/// Keep security-relevant settings the client did not mention.
///
/// The settings PUT replaces the whole document and `AppSettings` deserializes
/// with `#[serde(default)]`, so a field a client omits is indistinguishable
/// from one it reset. That is harmless for presentation settings and dangerous
/// for `self_update`: an operator who deliberately forbade console updates
/// would have that silently undone by any unrelated save from a client built
/// before the field existed — including a published CLI, or a stale browser
/// tab. Absence therefore means "leave it alone", and only an explicit value
/// changes it.
fn preserve_omitted_security_fields(incoming: &mut AppSettings, current: &AppSettings) {
    if incoming.self_update.is_none() {
        incoming.self_update = current.self_update.clone();
    }
}

/// Trim and validate an optional URL setting (`external_url`/`internal_url`).
/// A blank value (after trimming) means "unset" and is normalized to `None`
/// rather than rejected -- `external_url` previously validated the raw
/// `Some("")` a client sends for a cleared field and rejected it with
/// "must start with http:// or https://", which made every settings save
/// fail with a 400 on any instance that had never configured it. Kept as a
/// small pure helper (shared by both URL fields) so the two can't drift
/// apart again and the invariant is unit-testable.
fn sanitize_optional_url(
    field_label: &str,
    url: Option<String>,
) -> Result<Option<String>, Problem> {
    let Some(raw) = url else {
        return Ok(None);
    };

    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail(format!(
                "{field_label} URL must start with http:// or https://"
            ))
            .build());
    }
    if trimmed.contains('#') || trimmed.contains('?') {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail(format!(
                "{field_label} URL must not contain '#' or '?' characters"
            ))
            .build());
    }
    if url::Url::parse(&trimmed).is_err() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail(format!("{field_label} URL is not a valid URL"))
            .build());
    }

    Ok(Some(trimmed))
}

fn validate_observability_compression(
    compression: &ObservabilityCompressionSettings,
) -> Result<(), Problem> {
    if !(1..=720).contains(&compression.proxy_logs_after_hours) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("observability_compression.proxy_logs_after_hours must be between 1 and 720")
            .build());
    }
    if !(1..=2160).contains(&compression.otel_spans_after_hours) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("observability_compression.otel_spans_after_hours must be between 1 and 2160")
            .build());
    }
    Ok(())
}

/// Reject a chat turn timeout outside the supported range.
///
/// The runtime clamps on read, so an out-of-range value could never break the
/// chat — but storing one means the settings API echoes back a number that is
/// not what is in effect, and the form then shows the operator a limit that
/// isn't real. Rejecting keeps the stored value and the effective value the
/// same thing, which is the only way the page can be trusted.
fn validate_ai_chat_limits(limits: &AiChatLimitsSettings) -> Result<(), Problem> {
    let min = AiChatLimitsSettings::MIN_TURN_TIMEOUT_SECS;
    let max = AiChatLimitsSettings::MAX_TURN_TIMEOUT_SECS;
    if !(min..=max).contains(&limits.turn_timeout_secs) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Validation Error")
            .detail(format!(
                "ai_chat_limits.turn_timeout_secs must be between {min} and {max} seconds \
                 (got {})",
                limits.turn_timeout_secs
            ))
            .build());
    }
    Ok(())
}

fn validate_monitoring_settings(monitoring: &MonitoringSettings) -> Result<(), Problem> {
    if monitoring.scrape_interval_secs < 15 {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("monitoring.scrape_interval_secs must be >= 15")
            .build());
    }
    if !(1..=30).contains(&monitoring.retention_raw_days) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("monitoring.retention_raw_days must be between 1 and 30")
            .build());
    }
    if !(7..=365).contains(&monitoring.retention_hourly_days) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("monitoring.retention_hourly_days must be between 7 and 365")
            .build());
    }
    if !(1..=10).contains(&monitoring.retention_daily_years) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("monitoring.retention_daily_years must be between 1 and 10")
            .build());
    }
    // `monitoring.clickhouse_url` is legacy, optional config: the runtime
    // builds the ClickHouse metrics store from the server's TEMPS_CLICKHOUSE_*
    // env configuration (`build_ch_metrics_store`), never from this setting.
    // Requiring it when store == ClickHouse made the store unswitchable from
    // the UI (which has no URL field) even on servers where ClickHouse is
    // fully configured. Validate the URL only when one is supplied; the
    // env-not-configured case is surfaced by `effective_metrics_store` and
    // the console's mismatch warning instead of a save-time rejection.
    if let Some(url) = monitoring
        .clickhouse_url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
    {
        if url::Url::parse(url).is_err() {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .detail("monitoring.clickhouse_url is not a valid URL")
                .build());
        }
    }
    Ok(())
}

fn validate_observability_retention(
    retention: &ObservabilityRetentionSettings,
) -> Result<(), Problem> {
    let values = [
        ("proxy_logs_days", retention.proxy_logs_days),
        ("otel_spans_days", retention.otel_spans_days),
        ("otel_logs_days", retention.otel_logs_days),
        ("otel_metrics_days", retention.otel_metrics_days),
    ];
    for (field, days) in values {
        if !(1..=3650).contains(&days) {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .detail(format!(
                    "observability_retention.{field} must be between 1 and 3650"
                ))
                .build());
        }
    }
    Ok(())
}

/// Normalize the edge target: trim whitespace and treat an empty string as
/// `None` so an operator clearing the field disables DNS record sync.
fn normalize_edge_target(settings: &mut AppSettings) {
    if let Some(value) = settings.edge_target.take() {
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            settings.edge_target = Some(trimmed);
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
            preserve_omitted_security_fields(&mut settings, &current_settings);

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
            // re-supply it would otherwise wipe the stored DSN on an unrelated
            // save. Restore from the DB when absent.
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
            // Abort rather than proceed: the preservation block above did not
            // run, so saving now would silently overwrite every masked
            // sensitive field the client legitimately omitted (ClickHouse DSN,
            // preview-gateway shared_secret, join token hash, AI provider
            // credentials) with empty values. A failed save the operator can
            // retry is strictly better than an unannounced credential wipe.
            tracing::error!(
                "Could not fetch current settings to preserve sensitive fields; \
                 aborting settings save: {}",
                e
            );
            return Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Settings Save Aborted")
                .detail(format!(
                    "Could not load current settings to preserve masked sensitive \
                     fields (ClickHouse DSN, shared secrets, provider credentials); \
                     the save was aborted to avoid wiping them. Retry the save; if \
                     this persists, check database connectivity: {}",
                    e
                ))
                .build());
        }
    }

    if let Some(ref backend) = settings.agent_sandbox.sandbox_backend {
        let backend = backend.trim();
        if backend != "docker" && backend != "firecracker" {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Invalid Sandbox Backend")
                .detail(format!(
                    "sandbox_backend must be \"docker\" or \"firecracker\", got \"{}\"",
                    backend
                ))
                .build());
        }
    }

    validate_monitoring_settings(&settings.monitoring)?;
    validate_ai_chat_limits(&settings.ai_chat_limits)?;

    validate_observability_compression(&settings.observability_compression)?;
    validate_observability_retention(&settings.observability_retention)?;

    settings.external_url = sanitize_optional_url("External", settings.external_url)?;
    settings.internal_url = sanitize_optional_url("Internal", settings.internal_url)?;
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

    normalize_edge_target(&mut settings);

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
    hex::encode(digest)
}

#[derive(Debug, Clone, serde::Serialize)]
struct JoinTokenGeneratedAudit {
    context: AuditContext,
}

impl AuditOperation for JoinTokenGeneratedAudit {
    fn operation_type(&self) -> String {
        "JOIN_TOKEN_GENERATED".to_string()
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

#[derive(Debug, Clone, serde::Serialize)]
struct JoinTokenRevokedAudit {
    context: AuditContext,
}

impl AuditOperation for JoinTokenRevokedAudit {
    fn operation_type(&self) -> String {
        "JOIN_TOKEN_REVOKED".to_string()
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
        let mut rng = rand::rng();
        let bytes: Vec<u8> = (0..32).map(|_| rng.random::<u8>()).collect();
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
    use temps_core::{AgentSandboxSettings, AiChatLimitsSettings, AppSettings, ProviderConfig};

    /// An operator's decision to forbid console updates must survive a save
    /// from a client that has never heard of the field.
    ///
    /// `AppSettings` deserializes with `#[serde(default)]` and the PUT replaces
    /// the whole document, so an omitted `self_update` used to come back as
    /// "enabled" — silently re-arming the server's ability to replace its own
    /// binary. Regression test for that: absence means "leave it alone".
    #[test]
    fn omitting_self_update_preserves_the_stored_value() {
        let current = AppSettings {
            self_update: Some(temps_core::SelfUpdateSettings {
                enabled: false,
                channel: Some("stable".to_string()),
            }),
            ..AppSettings::default()
        };
        // What serde produces for a body that never mentioned the field.
        let mut incoming = AppSettings {
            self_update: None,
            ..AppSettings::default()
        };

        preserve_omitted_security_fields(&mut incoming, &current);

        let effective = incoming.self_update();
        assert!(
            !effective.enabled,
            "an omitted self_update must not re-enable console updates"
        );
        assert_eq!(effective.channel.as_deref(), Some("stable"));
    }

    /// An explicit value still wins — this is a preserve, not a freeze.
    #[test]
    fn an_explicit_self_update_value_overrides_the_stored_one() {
        let current = AppSettings {
            self_update: Some(temps_core::SelfUpdateSettings {
                enabled: false,
                channel: None,
            }),
            ..AppSettings::default()
        };
        let mut incoming = AppSettings {
            self_update: Some(temps_core::SelfUpdateSettings {
                enabled: true,
                channel: Some("beta".to_string()),
            }),
            ..AppSettings::default()
        };

        preserve_omitted_security_fields(&mut incoming, &current);

        let effective = incoming.self_update();
        assert!(effective.enabled);
        assert_eq!(effective.channel.as_deref(), Some("beta"));
    }

    /// A never-configured install reads as the documented default.
    #[test]
    fn absent_self_update_reads_as_enabled_by_default() {
        let settings = AppSettings::default();
        assert!(settings.self_update.is_none());
        assert!(settings.self_update().enabled);
        assert_eq!(settings.self_update().channel, None);
    }

    /// The stored value and the effective value must be the same number.
    ///
    /// The runtime clamps on read, so an out-of-range value could never break
    /// the chat — but it would be echoed back by the API and shown in the form,
    /// telling the operator a limit is in force that isn't. Found by testing
    /// the endpoint rather than trusting the clamp.
    #[test]
    fn ai_chat_turn_timeout_outside_the_supported_range_is_rejected() {
        for bad in [0, 5, 29, 3601, 99_999] {
            let limits = AiChatLimitsSettings {
                turn_timeout_secs: bad,
            };
            assert!(
                validate_ai_chat_limits(&limits).is_err(),
                "{bad}s should be rejected"
            );
        }
    }

    #[test]
    fn ai_chat_turn_timeout_within_range_is_accepted() {
        for ok in [30, 120, 900, 3600] {
            let limits = AiChatLimitsSettings {
                turn_timeout_secs: ok,
            };
            assert!(
                validate_ai_chat_limits(&limits).is_ok(),
                "{ok}s should be accepted"
            );
        }
    }

    /// The bounds the form advertises must be the bounds the server enforces,
    /// or the UI silently sends values that 400.
    #[test]
    fn advertised_bounds_match_the_runtime_clamp() {
        let min = AiChatLimitsSettings {
            turn_timeout_secs: AiChatLimitsSettings::MIN_TURN_TIMEOUT_SECS,
        };
        let max = AiChatLimitsSettings {
            turn_timeout_secs: AiChatLimitsSettings::MAX_TURN_TIMEOUT_SECS,
        };
        assert_eq!(
            min.turn_timeout().as_secs(),
            u64::from(AiChatLimitsSettings::MIN_TURN_TIMEOUT_SECS)
        );
        assert_eq!(
            max.turn_timeout().as_secs(),
            u64::from(AiChatLimitsSettings::MAX_TURN_TIMEOUT_SECS)
        );
        assert!(validate_ai_chat_limits(&min).is_ok());
        assert!(validate_ai_chat_limits(&max).is_ok());
    }

    // Regression: a client round-tripping a never-configured external_url
    // sends `Some("")` (the form's empty-string default), which previously
    // fell through straight to the http/https prefix check and rejected
    // every settings save with a 400 on any instance that had never set it.
    #[test]
    fn sanitize_optional_url_treats_blank_as_unset() {
        assert_eq!(
            sanitize_optional_url("External", Some(String::new())).unwrap(),
            None
        );
        assert_eq!(
            sanitize_optional_url("External", Some("   ".to_string())).unwrap(),
            None
        );
        assert_eq!(sanitize_optional_url("External", None).unwrap(), None);
    }

    #[test]
    fn sanitize_optional_url_trims_and_strips_trailing_slash() {
        assert_eq!(
            sanitize_optional_url("External", Some("  https://example.com/  ".to_string()))
                .unwrap(),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn sanitize_optional_url_rejects_missing_scheme() {
        let err = sanitize_optional_url("External", Some("example.com".to_string())).unwrap_err();
        let detail = err.body.get("detail").and_then(|v| v.as_str()).unwrap();
        assert!(detail.contains("must start with http:// or https://"));
    }

    #[test]
    fn sanitize_optional_url_rejects_query_or_fragment() {
        assert!(
            sanitize_optional_url("External", Some("https://example.com?a=1".to_string())).is_err()
        );
        assert!(
            sanitize_optional_url("External", Some("https://example.com#frag".to_string()))
                .is_err()
        );
    }

    #[test]
    fn observability_compression_validation_accepts_supported_boundaries() {
        assert!(
            validate_observability_compression(&ObservabilityCompressionSettings {
                proxy_logs_after_hours: 1,
                otel_spans_after_hours: 1,
            })
            .is_ok()
        );
        assert!(
            validate_observability_compression(&ObservabilityCompressionSettings {
                proxy_logs_after_hours: 720,
                otel_spans_after_hours: 2160,
            })
            .is_ok()
        );
    }

    #[test]
    fn observability_compression_validation_rejects_zero_and_over_retention() {
        assert!(
            validate_observability_compression(&ObservabilityCompressionSettings {
                proxy_logs_after_hours: 0,
                otel_spans_after_hours: 24,
            })
            .is_err()
        );
        assert!(
            validate_observability_compression(&ObservabilityCompressionSettings {
                proxy_logs_after_hours: 24,
                otel_spans_after_hours: 2161,
            })
            .is_err()
        );
    }

    #[test]
    fn observability_retention_validation_accepts_supported_boundaries() {
        assert!(
            validate_observability_retention(&ObservabilityRetentionSettings {
                proxy_logs_days: 1,
                otel_spans_days: 3650,
                otel_logs_days: 90,
                otel_metrics_days: 90,
            })
            .is_ok()
        );
    }

    #[test]
    fn observability_retention_validation_rejects_invalid_table_window() {
        let error = validate_observability_retention(&ObservabilityRetentionSettings {
            proxy_logs_days: 30,
            otel_spans_days: 90,
            otel_logs_days: 0,
            otel_metrics_days: 90,
        })
        .expect_err("zero-day retention must be rejected");
        assert_eq!(
            error.body.get("detail").and_then(|value| value.as_str()),
            Some("observability_retention.otel_logs_days must be between 1 and 3650")
        );
    }

    // The ClickHouse metrics store is built from TEMPS_CLICKHOUSE_* env
    // config, not from monitoring.clickhouse_url — selecting the ClickHouse
    // store without a settings-level URL must be a valid save (the UI has no
    // URL field; env-not-configured is reported via effective_metrics_store).
    #[test]
    fn monitoring_validation_accepts_clickhouse_store_without_url() {
        let monitoring = MonitoringSettings {
            store: MetricsStoreKind::ClickHouse,
            clickhouse_url: None,
            ..Default::default()
        };
        assert!(validate_monitoring_settings(&monitoring).is_ok());
    }

    #[test]
    fn monitoring_validation_rejects_malformed_clickhouse_url() {
        let monitoring = MonitoringSettings {
            store: MetricsStoreKind::ClickHouse,
            clickhouse_url: Some("not a url".into()),
            ..Default::default()
        };
        let error = validate_monitoring_settings(&monitoring)
            .expect_err("malformed clickhouse_url must be rejected");
        assert_eq!(
            error.body.get("detail").and_then(|value| value.as_str()),
            Some("monitoring.clickhouse_url is not a valid URL")
        );
    }

    #[test]
    fn monitoring_validation_accepts_daily_retention_boundaries() {
        for years in [1, 10] {
            let monitoring = MonitoringSettings {
                retention_daily_years: years,
                ..Default::default()
            };
            assert!(validate_monitoring_settings(&monitoring).is_ok());
        }
    }

    #[test]
    fn monitoring_validation_rejects_daily_retention_outside_supported_range() {
        for years in [0, 11] {
            let monitoring = MonitoringSettings {
                retention_daily_years: years,
                ..Default::default()
            };
            let error = validate_monitoring_settings(&monitoring)
                .expect_err("daily retention outside 1–10 years must be rejected");
            assert_eq!(
                error.body.get("detail").and_then(|value| value.as_str()),
                Some("monitoring.retention_daily_years must be between 1 and 10")
            );
        }
    }

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
                        max_turns_analysis: None,
                        max_turns_fix: None,
                        max_turns_feedback: None,
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
                sandbox_backend: None,
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
                max_turns_analysis: None,
                max_turns_fix: None,
                max_turns_feedback: None,
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

    #[test]
    fn response_surfaces_observability_compression_settings() {
        let mut settings = AppSettings::default();
        settings.observability_compression.proxy_logs_after_hours = 12;
        settings.observability_compression.otel_spans_after_hours = 48;

        let response = AppSettingsResponse::from(settings);

        assert_eq!(
            response.observability_compression.proxy_logs_after_hours,
            12
        );
        assert_eq!(
            response.observability_compression.otel_spans_after_hours,
            48
        );
    }

    #[test]
    fn response_surfaces_observability_retention_settings() {
        let mut settings = AppSettings::default();
        settings.observability_retention.proxy_logs_days = 14;
        settings.observability_retention.otel_spans_days = 60;

        let response = AppSettingsResponse::from(settings);

        assert_eq!(response.observability_retention.proxy_logs_days, 14);
        assert_eq!(response.observability_retention.otel_spans_days, 60);
        assert_eq!(response.observability_retention.otel_logs_days, 90);
        assert_eq!(response.observability_retention.otel_metrics_days, 90);
    }

    #[test]
    fn response_uses_active_timescale_policies_and_service_count() {
        let policies = EffectiveTelemetryPolicies {
            metrics_raw_days: Some(14),
            metrics_hourly_days: Some(120),
            metrics_daily_years: Some(3),
            proxy_logs_compression_hours: Some(12),
            otel_spans_compression_hours: Some(18),
            proxy_logs_retention_days: Some(21),
            otel_spans_retention_days: Some(75),
            otel_logs_retention_days: Some(45),
            otel_metrics_retention_days: Some(60),
        };

        let response = AppSettingsResponse::from(AppSettings::default())
            .with_effective_store(false)
            .with_effective_timescale_state(policies, Some(7));

        assert_eq!(response.monitored_services_count, Some(7));
        assert_eq!(response.monitoring.retention_raw_days, 14);
        assert_eq!(response.monitoring.retention_hourly_days, 120);
        assert_eq!(response.monitoring.retention_daily_years, 3);
        assert_eq!(
            response.observability_compression.proxy_logs_after_hours,
            12
        );
        assert_eq!(
            response.observability_compression.otel_spans_after_hours,
            18
        );
        assert_eq!(response.observability_retention.proxy_logs_days, 21);
        assert_eq!(response.observability_retention.otel_spans_days, 75);
        assert_eq!(response.observability_retention.otel_logs_days, 45);
        assert_eq!(response.observability_retention.otel_metrics_days, 60);
    }

    #[test]
    fn response_does_not_overlay_clickhouse_backed_values() {
        let mut settings = AppSettings::default();
        settings.monitoring.store = MetricsStoreKind::ClickHouse;
        let configured_monitoring = settings.monitoring.clone();
        let configured_compression = settings.observability_compression.clone();
        let configured_proxy_retention = settings.observability_retention.proxy_logs_days;
        let configured_span_retention = settings.observability_retention.otel_spans_days;
        let configured_metric_retention = settings.observability_retention.otel_metrics_days;

        let response = AppSettingsResponse::from(settings)
            .with_effective_store(true)
            .with_effective_timescale_state(
                EffectiveTelemetryPolicies {
                    metrics_raw_days: Some(1),
                    proxy_logs_compression_hours: Some(1),
                    otel_spans_compression_hours: Some(1),
                    proxy_logs_retention_days: Some(1),
                    otel_spans_retention_days: Some(1),
                    otel_logs_retention_days: Some(45),
                    otel_metrics_retention_days: Some(60),
                    ..Default::default()
                },
                Some(3),
            );

        assert_eq!(
            response.monitoring.retention_raw_days,
            configured_monitoring.retention_raw_days
        );
        assert_eq!(
            response.monitoring.retention_hourly_days,
            configured_monitoring.retention_hourly_days
        );
        assert_eq!(
            response.monitoring.retention_daily_years,
            configured_monitoring.retention_daily_years
        );
        assert_eq!(response.observability_compression, configured_compression);
        assert_eq!(
            response.observability_retention.proxy_logs_days,
            configured_proxy_retention
        );
        assert_eq!(
            response.observability_retention.otel_spans_days,
            configured_span_retention
        );
        assert_eq!(response.observability_retention.otel_logs_days, 45);
        assert_eq!(
            response.observability_retention.otel_metrics_days,
            configured_metric_retention
        );
        assert_eq!(response.monitored_services_count, Some(3));
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
        assert_eq!(
            response.effective_observability_store,
            MetricsStoreKind::TimescaleDb
        );

        // store=click_house AND env vars configured → runtime uses ClickHouse.
        let response = AppSettingsResponse::from(settings).with_effective_store(true);
        assert_eq!(
            response.effective_metrics_store,
            MetricsStoreKind::ClickHouse
        );
        assert_eq!(
            response.effective_observability_store,
            MetricsStoreKind::ClickHouse
        );

        // store=timescale_db → always TimescaleDB, regardless of env vars.
        let response = AppSettingsResponse::from(AppSettings::default()).with_effective_store(true);
        assert_eq!(
            response.effective_metrics_store,
            MetricsStoreKind::TimescaleDb
        );
        assert_eq!(
            response.effective_observability_store,
            MetricsStoreKind::ClickHouse,
            "proxy logs and spans use ClickHouse whenever its server config is available"
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
